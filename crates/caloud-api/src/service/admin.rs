//! 平臺後台：網域資源、子網域 ID、Runner 節點（ADR 0010、ADR 0011）。
//!
//! FR-OPS-03、FR-ENV-07、FR-OPS-04、FR-OPS-06。Admin API 與 Agent 端點都只經由這裡操作資料。
use super::normalize_hostname;
use crate::auth::hash_token;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::agent::{valid_node_name, NodeSpecs, ServerMsg, JOIN_PREFIX, NODE_PREFIX};
use caloud_core::db::audit;
use caloud_core::now;
use caloud_core::placement::{allocatable, NodeStatus, Resources};
use caloud_core::subdomain::{valid_reserved_name, validate_id, EnvKind, IdLengths, IdRules};
use serde::{Deserialize, Serialize};

/// 網域池用途
pub const PURPOSES: [&str; 4] = ["production", "staging", "verify", "system"];
/// 產生子網域 ID 的最大嘗試次數
const ID_ATTEMPTS: usize = 16;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::BadRequest(msg.into())
}

// ───────────────────────── 網域池 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DomainPool {
    pub root: String,
    pub purposes: Vec<String>,
    pub cloudflare_zone: Option<String>,
    /// 權杖末 4 碼；權杖本身只寫不讀（FR-OPS-03、FR-OPS-14）
    pub cloudflare_token_hint: Option<String>,
    pub id_lengths: IdLengths,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(sqlx::FromRow)]
struct PoolRow {
    root: String,
    purposes: String,
    cloudflare_zone: Option<String>,
    cloudflare_token_hint: Option<String>,
    id_lengths: String,
    created_at: i64,
    updated_at: i64,
}

impl From<PoolRow> for DomainPool {
    fn from(r: PoolRow) -> Self {
        DomainPool {
            root: r.root,
            purposes: serde_json::from_str(&r.purposes).unwrap_or_default(),
            cloudflare_zone: r.cloudflare_zone,
            cloudflare_token_hint: r.cloudflare_token_hint,
            id_lengths: serde_json::from_str(&r.id_lengths).unwrap_or_default(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// 網域池的輸入（POST 含 `root`，PUT 不含）
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolInput {
    pub root: Option<String>,
    pub purposes: Vec<String>,
    pub cloudflare_zone: Option<String>,
    /// 只寫：省略不變、空字串清除（FR-OPS-03）
    pub cloudflare_token: Option<String>,
    pub id_lengths: Option<IdLengths>,
}

/// 網域池權杖的變更
enum TokenChange {
    Keep,
    Clear,
    /// （密文, 提示, zone id）
    Set(String, String, String),
}

/// FR-OPS-03、FR-OPS-14：先經 Cloudflare 驗證權杖並取得 zone id，再以主金鑰加密
async fn token_change(s: &AppState, root: &str, zone: Option<&str>, input: &PoolInput) -> ApiResult<TokenChange> {
    let Some(token) = input.cloudflare_token.as_deref().map(str::trim) else {
        return Ok(TokenChange::Keep);
    };
    if token.is_empty() {
        return Ok(TokenChange::Clear);
    }
    let zone = zone.ok_or_else(|| bad("設定 Cloudflare 權杖前請先填寫 Cloudflare zone。"))?;
    let key = s
        .master_key
        .as_ref()
        .ok_or_else(|| bad("伺服器未設定主金鑰 CALOUD_MASTER_KEY，無法保存權杖。"))?;
    let cf = crate::cloudflare::Cloudflare {
        http: &s.http,
        base: &s.cfg.cloudflare_api,
        token,
    };
    let zone_id = cf.zone_id(zone).await.map_err(|e| bad(format!("{e}")))?;
    let enc = key
        .seal(crate::certs::TOKEN_PURPOSE, root, token.as_bytes())
        .map_err(ApiError::Internal)?;
    Ok(TokenChange::Set(enc, caloud_core::secret::hint(token), zone_id))
}

async fn apply_token(s: &AppState, root: &str, change: TokenChange) -> ApiResult<Option<&'static str>> {
    let (enc, hint, zone_id, detail) = match change {
        TokenChange::Keep => return Ok(None),
        TokenChange::Clear => (None, None, None, "cloudflare_token cleared"),
        TokenChange::Set(e, h, z) => (Some(e), Some(h), Some(z), "cloudflare_token set"),
    };
    sqlx::query("UPDATE domain_pools SET cloudflare_token_enc = ?, cloudflare_token_hint = ?, cloudflare_zone_id = ? WHERE root = ?")
        .bind(enc)
        .bind(hint)
        .bind(zone_id)
        .bind(root)
        .execute(&s.db)
        .await?;
    crate::certs::kick(s);
    Ok(Some(detail))
}

async fn pools(s: &AppState) -> ApiResult<Vec<DomainPool>> {
    let rows: Vec<PoolRow> = sqlx::query_as("SELECT * FROM domain_pools ORDER BY root").fetch_all(&s.db).await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// FR-OPS-03
pub async fn list_pools(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<DomainPool>> {
    me.require_admin()?;
    pools(s).await
}

/// 檢查用途、長度、zone；環境用途（production／staging／verify）只能屬於一個網域池
async fn check_pool(s: &AppState, root: &str, input: &PoolInput) -> ApiResult<(Vec<String>, IdLengths, Option<String>)> {
    let mut purposes: Vec<String> = Vec::new();
    for p in &input.purposes {
        if !PURPOSES.contains(&p.as_str()) {
            return Err(bad(format!("用途 `{p}` 不存在，可用：production、staging、verify、system。")));
        }
        if !purposes.contains(p) {
            purposes.push(p.clone());
        }
    }
    if purposes.is_empty() {
        return Err(bad("請至少選擇一種用途。"));
    }
    let lengths = input.id_lengths.unwrap_or_default();
    lengths.check().map_err(bad)?;
    let zone = match input.cloudflare_zone.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(z) => Some(normalize_hostname(z)?),
    };
    for other in pools(s).await?.into_iter().filter(|p| p.root != root) {
        if let Some(p) = purposes.iter().find(|p| *p != "system" && other.purposes.contains(p)) {
            return Err(ApiError::Conflict(format!(
                "用途 `{p}` 已由網域池 {} 使用，請先從該網域池移除。",
                other.root
            )));
        }
    }
    Ok((purposes, lengths, zone))
}

async fn get_pool(s: &AppState, root: &str) -> ApiResult<DomainPool> {
    let row: Option<PoolRow> = sqlx::query_as("SELECT * FROM domain_pools WHERE root = ?")
        .bind(root)
        .fetch_optional(&s.db)
        .await?;
    row.map(Into::into).ok_or(ApiError::NotFound)
}

/// FR-OPS-03
pub async fn create_pool(s: &AppState, me: &CurrentUser, input: PoolInput) -> ApiResult<DomainPool> {
    me.require_admin()?;
    let root = normalize_hostname(input.root.as_deref().unwrap_or_default())?;
    let (purposes, lengths, zone) = check_pool(s, &root, &input).await?;
    if pools(s).await?.iter().any(|p| p.root == root) {
        return Err(ApiError::Conflict(format!("網域池 {root} 已存在。")));
    }
    let change = token_change(s, &root, zone.as_deref(), &input).await?;
    let t = now();
    let r = sqlx::query(
        "INSERT INTO domain_pools (root, purposes, cloudflare_zone, id_lengths, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(root) DO NOTHING",
    )
    .bind(&root)
    .bind(serde_json::to_string(&purposes).unwrap_or_default())
    .bind(&zone)
    .bind(serde_json::to_string(&lengths).unwrap_or_default())
    .bind(t)
    .bind(t)
    .execute(&s.db)
    .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::Conflict(format!("網域池 {root} 已存在。")));
    }
    let detail = apply_token(s, &root, change).await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_pool_create",
        Some(&root),
        me.audit_detail(detail).as_deref(),
    )
    .await;
    get_pool(s, &root).await
}

/// FR-OPS-03
pub async fn update_pool(s: &AppState, me: &CurrentUser, root: &str, input: PoolInput) -> ApiResult<DomainPool> {
    me.require_admin()?;
    if input.root.is_some() {
        return Err(bad("根網域不可修改，請新增另一個網域池。"));
    }
    let old = get_pool(s, &root.to_ascii_lowercase()).await?;
    let root = old.root.clone();
    let (purposes, lengths, zone) = check_pool(s, &root, &input).await?;
    let mut change = token_change(s, &root, zone.as_deref(), &input).await?;
    if matches!(change, TokenChange::Keep) && old.cloudflare_token_hint.is_some() && zone != old.cloudflare_zone {
        if zone.is_none() {
            change = TokenChange::Clear;
        } else {
            return Err(bad("變更 Cloudflare zone 時請重新輸入權杖。"));
        }
    }
    sqlx::query("UPDATE domain_pools SET purposes = ?, cloudflare_zone = ?, id_lengths = ?, updated_at = ? WHERE root = ?")
        .bind(serde_json::to_string(&purposes).unwrap_or_default())
        .bind(&zone)
        .bind(serde_json::to_string(&lengths).unwrap_or_default())
        .bind(now())
        .bind(&root)
        .execute(&s.db)
        .await?;
    let detail = apply_token(s, &root, change).await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_pool_update",
        Some(&root),
        me.audit_detail(detail).as_deref(),
    )
    .await;
    get_pool(s, &root).await
}

/// FR-OPS-03
pub async fn delete_pool(s: &AppState, me: &CurrentUser, root: &str) -> ApiResult<()> {
    me.require_admin()?;
    let root = root.to_ascii_lowercase();
    let r = sqlx::query("DELETE FROM domain_pools WHERE root = ?")
        .bind(&root)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_pool_delete",
        Some(&root),
        me.audit_detail(None).as_deref(),
    )
    .await;
    crate::certs::kick(s);
    Ok(())
}

/// FR-OPS-10、FR-DOM-05（ADR 0016）
pub async fn list_certs(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<crate::certs::Certificate>> {
    me.require_admin()?;
    Ok(crate::certs::list(s).await?)
}

// ───────────────────────── 保留字 ─────────────────────────

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ReservedName {
    pub name: String,
    pub created_at: i64,
}

/// FR-OPS-03
pub async fn list_reserved(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<ReservedName>> {
    me.require_admin()?;
    Ok(sqlx::query_as("SELECT * FROM reserved_names ORDER BY name")
        .fetch_all(&s.db)
        .await?)
}

/// FR-OPS-03
pub async fn add_reserved(s: &AppState, me: &CurrentUser, name: &str) -> ApiResult<ReservedName> {
    me.require_admin()?;
    let name = name.trim().to_ascii_lowercase();
    if !valid_reserved_name(&name) {
        return Err(bad("保留字為 1–32 個字元，只能使用小寫英文字母、數字與 -。"));
    }
    let row: Option<ReservedName> =
        sqlx::query_as("INSERT INTO reserved_names (name, created_at) VALUES (?, ?) ON CONFLICT(name) DO NOTHING RETURNING *")
            .bind(&name)
            .bind(now())
            .fetch_optional(&s.db)
            .await?;
    let row = row.ok_or_else(|| ApiError::Conflict(format!("保留字 {name} 已存在。")))?;
    audit(
        &s.db,
        Some(&me.0.account),
        "reserved_name_add",
        Some(&name),
        me.audit_detail(None).as_deref(),
    )
    .await;
    Ok(row)
}

/// FR-OPS-03
pub async fn remove_reserved(s: &AppState, me: &CurrentUser, name: &str) -> ApiResult<()> {
    me.require_admin()?;
    let r = sqlx::query("DELETE FROM reserved_names WHERE name = ?")
        .bind(name)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit(
        &s.db,
        Some(&me.0.account),
        "reserved_name_remove",
        Some(name),
        me.audit_detail(None).as_deref(),
    )
    .await;
    Ok(())
}

// ───────────────────────── 封鎖清單 ─────────────────────────

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct BlockedHost {
    pub hostname: String,
    pub reason: String,
    pub created_by: String,
    pub created_at: i64,
}

/// FR-OPS-03
pub async fn list_blocked(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<BlockedHost>> {
    me.require_admin()?;
    Ok(sqlx::query_as("SELECT * FROM blocked_hosts ORDER BY created_at DESC")
        .fetch_all(&s.db)
        .await?)
}

/// FR-OPS-03
pub async fn block_host(s: &AppState, me: &CurrentUser, hostname: &str, reason: &str) -> ApiResult<BlockedHost> {
    me.require_admin()?;
    let hostname = normalize_hostname(hostname)?;
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 200 {
        return Err(bad("請填寫 1–200 字的封鎖理由。"));
    }
    let row: Option<BlockedHost> = sqlx::query_as(
        "INSERT INTO blocked_hosts (hostname, reason, created_by, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(hostname) DO NOTHING RETURNING *",
    )
    .bind(&hostname)
    .bind(reason)
    .bind(&me.0.account)
    .bind(now())
    .fetch_optional(&s.db)
    .await?;
    let row = row.ok_or_else(|| ApiError::Conflict(format!("{hostname} 已在封鎖清單中。")))?;
    audit(
        &s.db,
        Some(&me.0.account),
        "host_block",
        Some(&hostname),
        me.audit_detail(Some(reason)).as_deref(),
    )
    .await;
    Ok(row)
}

/// FR-OPS-03
pub async fn unblock_host(s: &AppState, me: &CurrentUser, hostname: &str) -> ApiResult<()> {
    me.require_admin()?;
    let hostname = hostname.to_ascii_lowercase();
    let r = sqlx::query("DELETE FROM blocked_hosts WHERE hostname = ?")
        .bind(&hostname)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit(
        &s.db,
        Some(&me.0.account),
        "host_unblock",
        Some(&hostname),
        me.audit_detail(None).as_deref(),
    )
    .await;
    Ok(())
}

// ───────────────────────── 子網域 ID ─────────────────────────

/// FR-ENV-07：依用途的網域池產生一個目前可用的 ID，回傳 `(id, hostname)`；不保留
pub async fn new_env_id(s: &AppState, kind: EnvKind) -> ApiResult<(String, String)> {
    let pool = pools(s)
        .await?
        .into_iter()
        .find(|p| p.purposes.iter().any(|x| x == kind.as_str()))
        .ok_or_else(|| bad(format!("尚未設定給 {} 使用的網域池，請先到後台新增。", kind.as_str())))?;
    let reserved: Vec<String> = sqlx::query_scalar("SELECT name FROM reserved_names").fetch_all(&s.db).await?;
    let rules = IdRules {
        lengths: pool.id_lengths,
        reserved,
    };
    let len = rules.lengths.get(kind);
    for _ in 0..ID_ATTEMPTS {
        let id = caloud_auth::new_subdomain_id(len);
        if validate_id(&id, kind, &rules).is_err() {
            continue;
        }
        let hostname = format!("{id}.{}", pool.root);
        let taken: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM blocked_hosts WHERE hostname = ?1) + (SELECT COUNT(*) FROM domains WHERE hostname = ?1) \
             + (SELECT COUNT(*) FROM environments WHERE hostname = ?1)",
        )
        .bind(&hostname)
        .fetch_one(&s.db)
        .await?;
        if taken == 0 {
            return Ok((id, hostname));
        }
    }
    Err(ApiError::Conflict("無法產生可用的子網域 ID，請檢查保留字是否過多或過短。".into()))
}

/// FR-ENV-07：後台預覽一個可用 ID
pub async fn preview_env_id(s: &AppState, me: &CurrentUser, kind: &str) -> ApiResult<(String, String)> {
    me.require_admin()?;
    let kind = EnvKind::parse(kind).ok_or_else(|| bad("kind 需為 production、staging 或 verify。"))?;
    new_env_id(s, kind).await
}

// ───────────────────────── Runner 節點 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub status: String,
    pub specs: NodeSpecs,
    pub allocatable: Resources,
    pub allocated: Resources,
    pub used: Resources,
    pub last_seen: i64,
    pub created_at: i64,
}

#[derive(sqlx::FromRow)]
struct NodeRow {
    id: String,
    name: String,
    owner: Option<String>,
    specs: String,
    alloc_cpu: i64,
    alloc_mem: i64,
    used_cpu: i64,
    used_mem: i64,
    assigned_cpu: i64,
    assigned_mem: i64,
    status: String,
    last_seen: i64,
    created_at: i64,
}

fn res(cpu: i64, mem: i64) -> Resources {
    Resources {
        cpu_millis: cpu.max(0) as u64,
        memory_bytes: mem.max(0) as u64,
    }
}

impl TryFrom<NodeRow> for Node {
    type Error = ApiError;
    fn try_from(r: NodeRow) -> ApiResult<Self> {
        Ok(Node {
            specs: serde_json::from_str(&r.specs).map_err(|e| ApiError::Internal(e.into()))?,
            allocatable: res(r.alloc_cpu, r.alloc_mem),
            allocated: res(r.assigned_cpu, r.assigned_mem),
            used: res(r.used_cpu, r.used_mem),
            id: r.id,
            name: r.name,
            owner: r.owner,
            status: r.status,
            last_seen: r.last_seen,
            created_at: r.created_at,
        })
    }
}

pub(crate) async fn get_node(s: &AppState, id: &str) -> ApiResult<Node> {
    let row: Option<NodeRow> = sqlx::query_as("SELECT * FROM nodes WHERE id = ?")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    row.ok_or(ApiError::NotFound)?.try_into()
}

/// FR-OPS-04：平臺節點與自有設備全部列出（後台）
pub async fn list_nodes(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<Node>> {
    me.require_admin()?;
    let rows: Vec<NodeRow> = sqlx::query_as("SELECT * FROM nodes ORDER BY owner IS NOT NULL, name")
        .fetch_all(&s.db)
        .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// FR-OPS-04：一次性加入權杖（平臺節點），只在此回傳一次；回傳 `(token, expires_at)`
pub async fn create_join_token(s: &AppState, me: &CurrentUser, minutes: Option<i64>) -> ApiResult<(String, i64)> {
    me.require_admin()?;
    let minutes = minutes.unwrap_or(60);
    if !(1..=60).contains(&minutes) {
        return Err(bad("有效時間需介於 1 到 60 分鐘。"));
    }
    let token = format!("{JOIN_PREFIX}{}", caloud_auth::new_token());
    let expires_at = now() + minutes * 60;
    sqlx::query("INSERT INTO node_join_tokens (token_hash, owner, created_by, expires_at) VALUES (?, NULL, ?, ?)")
        .bind(hash_token(&token))
        .bind(&me.0.account)
        .bind(expires_at)
        .execute(&s.db)
        .await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "node_join_token_create",
        None,
        me.audit_detail(None).as_deref(),
    )
    .await;
    Ok((token, expires_at))
}

/// FR-OPS-06：排空或恢復節點，並通知已連線的 agent
pub async fn set_node_status(s: &AppState, me: &CurrentUser, id: &str, to: NodeStatus) -> ApiResult<Node> {
    me.require_admin()?;
    let node = get_node(s, id).await?;
    let to = match to {
        NodeStatus::Draining => NodeStatus::Draining,
        NodeStatus::Active => {
            if !s.agents.is_connected(id) {
                return Err(ApiError::Conflict("節點目前離線，重新連線後才能恢復接工作。".into()));
            }
            NodeStatus::Active
        }
        NodeStatus::Offline => return Err(bad("無法手動設為離線。")),
    };
    sqlx::query("UPDATE nodes SET status = ? WHERE id = ?")
        .bind(to.as_str())
        .bind(&node.id)
        .execute(&s.db)
        .await?;
    s.agents.send(
        &node.id,
        ServerMsg::Status {
            status: to.as_str().into(),
        },
    );
    let action = if to == NodeStatus::Draining {
        "node_drain"
    } else {
        "node_activate"
    };
    audit(
        &s.db,
        Some(&me.0.account),
        action,
        Some(&node.id),
        me.audit_detail(Some(&node.name)).as_deref(),
    )
    .await;
    get_node(s, id).await
}

/// FR-OPS-06：只有排空中或離線的節點可移除；移除後節點權杖失效並中斷連線
pub async fn delete_node(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<()> {
    me.require_admin()?;
    let node = get_node(s, id).await?;
    if node.status == NodeStatus::Active.as_str() {
        return Err(ApiError::Conflict("請先排空節點，等工作搬移完成後再移除。".into()));
    }
    sqlx::query("DELETE FROM nodes WHERE id = ?").bind(&node.id).execute(&s.db).await?;
    s.agents.disconnect(&node.id);
    s.tunnels.remove(&node.id, None);
    audit(
        &s.db,
        Some(&me.0.account),
        "node_delete",
        Some(&node.id),
        me.audit_detail(Some(&node.name)).as_deref(),
    )
    .await;
    Ok(())
}

/// agent 註冊時送來的規格是否合理（只用於調度，不作權限判斷）
fn check_specs(specs: &NodeSpecs) -> ApiResult<()> {
    let ok = (1..=4096).contains(&specs.cpu_cores)
        && specs.memory_bytes > 0
        && specs.arch.len() <= 32
        && specs.os.len() <= 128
        && specs.agent_version.len() <= 32;
    ok.then_some(()).ok_or_else(|| bad("節點規格格式錯誤。"))
}

/// FR-OPS-04：agent 註冊。以加入權杖首次註冊（回傳新的節點權杖），或以節點權杖重連
pub(crate) async fn register_node(
    s: &AppState,
    join_token: Option<&str>,
    node_token: Option<&str>,
    name: &str,
    specs: &NodeSpecs,
) -> ApiResult<(Node, Option<String>)> {
    if !valid_node_name(name) {
        return Err(bad("節點名稱為 1–63 個字元，只能使用小寫英文字母、數字與 -。"));
    }
    check_specs(specs)?;
    let alloc = allocatable(specs.cpu_cores, specs.memory_bytes);
    let specs_json = serde_json::to_string(specs).map_err(|e| ApiError::Internal(e.into()))?;
    let t = now();
    match (join_token, node_token) {
        (Some(join), None) if join.starts_with(JOIN_PREFIX) => {
            let hash = hash_token(join);
            // 不論成功與否，權杖都作廢；只有原本未使用且未過期才算有效
            let row: Option<(Option<String>, String, i64, Option<i64>)> =
                sqlx::query_as("SELECT owner, created_by, expires_at, used_at FROM node_join_tokens WHERE token_hash = ?")
                    .bind(&hash)
                    .fetch_optional(&s.db)
                    .await?;
            let used = sqlx::query("UPDATE node_join_tokens SET used_at = ? WHERE token_hash = ? AND used_at IS NULL")
                .bind(t)
                .bind(&hash)
                .execute(&s.db)
                .await?
                .rows_affected();
            let Some((owner, created_by, expires_at, None)) = row else {
                return Err(ApiError::Unauthorized);
            };
            if used != 1 || expires_at <= t {
                return Err(ApiError::Unauthorized);
            }
            let id = uuid::Uuid::new_v4().to_string();
            let token = format!("{NODE_PREFIX}{}", caloud_auth::new_token());
            sqlx::query(
                "INSERT INTO nodes (id, name, owner, specs, alloc_cpu, alloc_mem, status, token_hash, last_seen, created_at) VALUES (?, ?, ?, ?, ?, ?, 'active', ?, ?, ?)",
            )
            .bind(&id)
            .bind(name)
            .bind(&owner)
            .bind(&specs_json)
            .bind(alloc.cpu_millis as i64)
            .bind(alloc.memory_bytes as i64)
            .bind(hash_token(&token))
            .bind(t)
            .bind(t)
            .execute(&s.db)
            .await?;
            audit(&s.db, Some(&created_by), "node_join", Some(&id), Some(name)).await;
            Ok((get_node(s, &id).await?, Some(token)))
        }
        (None, Some(node)) if node.starts_with(NODE_PREFIX) => {
            let id: Option<String> = sqlx::query_scalar(
                "UPDATE nodes SET name = ?, specs = ?, alloc_cpu = ?, alloc_mem = ?, last_seen = ?, \
                 status = CASE WHEN status = 'offline' THEN 'active' ELSE status END WHERE token_hash = ? RETURNING id",
            )
            .bind(name)
            .bind(&specs_json)
            .bind(alloc.cpu_millis as i64)
            .bind(alloc.memory_bytes as i64)
            .bind(t)
            .bind(hash_token(node))
            .fetch_optional(&s.db)
            .await?;
            let id = id.ok_or(ApiError::Unauthorized)?;
            Ok((get_node(s, &id).await?, None))
        }
        _ => Err(ApiError::Unauthorized),
    }
}

/// 通道驗證（docs/06 §6.1）：節點權杖 → 節點 id（節點必須存在）
pub(crate) async fn node_by_token(s: &AppState, token: &str) -> ApiResult<String> {
    if !token.starts_with(NODE_PREFIX) {
        return Err(ApiError::Unauthorized);
    }
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM nodes WHERE token_hash = ?")
        .bind(hash_token(token))
        .fetch_optional(&s.db)
        .await?;
    id.ok_or(ApiError::Unauthorized)
}

/// FR-OPS-04：心跳。回傳節點是否仍存在（已移除則 agent 應斷線）
pub(crate) async fn heartbeat(s: &AppState, node_id: &str, used: Resources) -> ApiResult<bool> {
    let r = sqlx::query(
        "UPDATE nodes SET used_cpu = ?, used_mem = ?, last_seen = ?, \
         status = CASE WHEN status = 'offline' THEN 'active' ELSE status END WHERE id = ?",
    )
    .bind(used.cpu_millis.min(i64::MAX as u64) as i64)
    .bind(used.memory_bytes.min(i64::MAX as u64) as i64)
    .bind(now())
    .bind(node_id)
    .execute(&s.db)
    .await?;
    Ok(r.rows_affected() == 1)
}

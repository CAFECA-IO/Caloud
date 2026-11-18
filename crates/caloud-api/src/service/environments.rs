//! 平臺環境（FR-OPS-13，ADR 0015）：維運者在 Runner 開出、列出、刪除環境。
//!
//! 控制面配置子網域 ID、節點與網段索引，經 agent 連線送出 `deploy`／`destroy`；
//! agent 以 `env_status` 回報，running 的環境由 Edge 經通道轉發（M4-07）。
use super::admin::{get_node, new_env_id};
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::agent::{EnvService, EnvSpec, ServerMsg};
use caloud_core::db::audit;
use caloud_core::now;
use caloud_core::placement::{choose_node, Candidate, NodeStatus, Resources, Target};
use caloud_core::subdomain::EnvKind;
use caloud_orchestrator::sandbox::{self, MAX_ALLOC_INDEX};
use caloud_orchestrator::{validate_with, Policy};
use serde::{Deserialize, Serialize};

/// 調度時每個服務保留的量（上限見 `sandbox::Limits`：1 核、1 GB；保留量較低，允許適度超賣）
pub const PER_SERVICE: Resources = Resources {
    cpu_millis: 250,
    memory_bytes: 512 * 1024 * 1024,
};

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::BadRequest(msg.into())
}

#[derive(Debug, Clone, Serialize)]
pub struct Environment {
    pub subdomain: String,
    pub hostname: String,
    pub kind: String,
    pub node: String,
    pub subnet_index: i64,
    pub status: String,
    pub error: Option<String>,
    pub public_service: String,
    pub public_port: i64,
    pub services: Vec<EnvService>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    subdomain: String,
    hostname: String,
    kind: String,
    node: String,
    subnet_index: i64,
    status: String,
    error: Option<String>,
    public_service: String,
    public_port: i64,
    services: String,
    created_by: String,
    created_at: i64,
    updated_at: i64,
}

impl From<Row> for Environment {
    fn from(r: Row) -> Self {
        Self {
            services: serde_json::from_str(&r.services).unwrap_or_default(),
            subdomain: r.subdomain,
            hostname: r.hostname,
            kind: r.kind,
            node: r.node,
            subnet_index: r.subnet_index,
            status: r.status,
            error: r.error,
            public_service: r.public_service,
            public_port: r.public_port,
            created_by: r.created_by,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

const COLUMNS: &str = "subdomain, hostname, kind, node, subnet_index, status, error, public_service, public_port, services, created_by, created_at, updated_at";

async fn get(s: &AppState, subdomain: &str) -> ApiResult<Environment> {
    let row: Option<Row> = sqlx::query_as(&format!("SELECT {COLUMNS} FROM environments WHERE subdomain = ?"))
        .bind(subdomain)
        .fetch_optional(&s.db)
        .await?;
    Ok(row.ok_or(ApiError::NotFound)?.into())
}

pub async fn list(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<Environment>> {
    me.require_admin()?;
    let rows: Vec<Row> = sqlx::query_as(&format!("SELECT {COLUMNS} FROM environments ORDER BY created_at DESC"))
        .fetch_all(&s.db)
        .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[derive(Debug, Deserialize)]
pub struct EnvInput {
    pub kind: String,
    pub compose_yaml: String,
    pub public_service: String,
    pub public_port: i64,
    pub node_id: Option<String>,
}

/// 重新計算節點的已配置量（環境合計）
async fn refresh_assigned(s: &AppState, node: &str) -> ApiResult<()> {
    sqlx::query(
        "UPDATE nodes SET assigned_cpu = (SELECT COALESCE(SUM(cpu_millis), 0) FROM environments WHERE node = ?1), \
         assigned_mem = (SELECT COALESCE(SUM(memory_bytes), 0) FROM environments WHERE node = ?1) WHERE id = ?1",
    )
    .bind(node)
    .execute(&s.db)
    .await?;
    Ok(())
}

/// 平臺節點中通過自我檢查者（FR-OPS-12）；可限定節點
async fn candidates(s: &AppState, only: Option<&str>) -> ApiResult<Vec<Candidate>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM nodes WHERE owner IS NULL ORDER BY id")
        .fetch_all(&s.db)
        .await?;
    let mut out = Vec::new();
    for (id,) in rows {
        if only.is_some_and(|o| o != id) {
            continue;
        }
        let n = get_node(s, &id).await?;
        if !n.specs.isolation || !s.agents.is_connected(&n.id) {
            continue;
        }
        out.push(Candidate {
            id: n.id,
            owner: n.owner,
            status: NodeStatus::parse(&n.status).unwrap_or(NodeStatus::Offline),
            allocatable: n.allocatable,
            allocated: n.allocated,
        });
    }
    Ok(out)
}

/// FR-OPS-13：開出環境
pub async fn create(s: &AppState, me: &CurrentUser, input: EnvInput) -> ApiResult<Environment> {
    me.require_admin()?;
    let kind = EnvKind::parse(&input.kind).ok_or_else(|| bad("kind 需為 production、staging 或 verify。"))?;
    if input.compose_yaml.len() > 256 * 1024 {
        return Err(bad("compose 過長（上限 256 KB）。"));
    }
    let compose = validate_with(&input.compose_yaml, Policy::platform(0)).map_err(|e| bad(e.to_string()))?;
    sandbox::check_image_only(&compose).map_err(bad)?;
    if !compose.services.contains(&input.public_service) {
        return Err(bad(format!("compose 中沒有服務 `{}`。", input.public_service)));
    }
    if !(1..=65535).contains(&input.public_port) {
        return Err(bad("public_port 需介於 1 到 65535。"));
    }
    let n = compose.services.len() as u64;
    let need = Resources {
        cpu_millis: PER_SERVICE.cpu_millis * n,
        memory_bytes: PER_SERVICE.memory_bytes * n,
    };
    let nodes = candidates(s, input.node_id.as_deref()).await?;
    let node = choose_node(&nodes, need, Target::Platform)
        .ok_or_else(|| ApiError::Conflict("沒有可用的 Runner 節點：需要在線、通過自我檢查且容量足夠的平臺節點。".into()))?
        .id
        .clone();
    let used: Vec<i64> = sqlx::query_scalar("SELECT subnet_index FROM environments WHERE node = ? ORDER BY subnet_index")
        .bind(&node)
        .fetch_all(&s.db)
        .await?;
    let index = (0..=i64::from(MAX_ALLOC_INDEX))
        .find(|i| used.binary_search(i).is_err())
        .ok_or_else(|| ApiError::Conflict("這個節點的環境網段已用完。".into()))?;
    let (subdomain, hostname) = new_env_id(s, kind).await?;
    let t = now();
    sqlx::query(
        "INSERT INTO environments (subdomain, hostname, kind, node, subnet_index, compose, public_service, public_port, cpu_millis, memory_bytes, status, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?)",
    )
    .bind(&subdomain)
    .bind(&hostname)
    .bind(kind.as_str())
    .bind(&node)
    .bind(index)
    .bind(&input.compose_yaml)
    .bind(&input.public_service)
    .bind(input.public_port)
    .bind(need.cpu_millis as i64)
    .bind(need.memory_bytes as i64)
    .bind(&me.0.account)
    .bind(t)
    .bind(t)
    .execute(&s.db)
    .await?;
    refresh_assigned(s, &node).await?;
    s.agents.send(
        &node,
        ServerMsg::Deploy {
            env: EnvSpec {
                id: subdomain.clone(),
                kind: kind.as_str().into(),
                compose: input.compose_yaml.clone(),
                subnet_index: index as u16,
                public_service: input.public_service.clone(),
            },
        },
    );
    audit(
        &s.db,
        Some(&me.0.account),
        "env_create",
        Some(&subdomain),
        me.audit_detail(Some(&hostname)).as_deref(),
    )
    .await;
    get(s, &subdomain).await
}

/// FR-OPS-13：刪除環境；節點在線時請 agent 清除，否則直接移除紀錄
pub async fn delete(s: &AppState, me: &CurrentUser, subdomain: &str) -> ApiResult<()> {
    me.require_admin()?;
    let env = get(s, subdomain).await?;
    if s.agents.is_connected(&env.node) {
        sqlx::query("UPDATE environments SET status = 'destroying', updated_at = ? WHERE subdomain = ?")
            .bind(now())
            .bind(subdomain)
            .execute(&s.db)
            .await?;
        s.agents.send(
            &env.node,
            ServerMsg::Destroy {
                env_id: env.subdomain.clone(),
            },
        );
    } else {
        remove(s, &env).await?;
    }
    reload_routes(s).await;
    audit(
        &s.db,
        Some(&me.0.account),
        "env_delete",
        Some(subdomain),
        me.audit_detail(Some(&env.hostname)).as_deref(),
    )
    .await;
    Ok(())
}

async fn remove(s: &AppState, env: &Environment) -> ApiResult<()> {
    sqlx::query("DELETE FROM environments WHERE subdomain = ?")
        .bind(&env.subdomain)
        .execute(&s.db)
        .await?;
    refresh_assigned(s, &env.node).await
}

async fn reload_routes(s: &AppState) {
    if let Err(e) = s.routes.reload(&s.db).await {
        tracing::error!(error = %e, "route reload failed");
    }
}

/// agent 回報（只接受該節點上的環境）
pub(crate) async fn report(
    s: &AppState,
    node: &str,
    env_id: &str,
    status: &str,
    error: Option<String>,
    services: Vec<EnvService>,
) -> ApiResult<()> {
    let env = match get(s, env_id).await {
        Ok(e) if e.node == node => e,
        _ => {
            tracing::warn!(%node, %env_id, "status for unknown environment");
            return Ok(());
        }
    };
    match status {
        "destroyed" => remove(s, &env).await?,
        "running" => {
            let ip = services.iter().find(|x| x.name == env.public_service).map(|x| x.ip.clone());
            let svc = serde_json::to_string(&services).unwrap_or_else(|_| "[]".into());
            sqlx::query("UPDATE environments SET status = 'running', error = NULL, public_ip = ?, services = ?, updated_at = ? WHERE subdomain = ? AND status = 'deploying'")
                .bind(ip)
                .bind(svc)
                .bind(now())
                .bind(env_id)
                .execute(&s.db)
                .await?;
        }
        _ => {
            let err: String = error.unwrap_or_else(|| "部署失敗".into()).chars().take(2000).collect();
            sqlx::query("UPDATE environments SET status = 'failed', error = ?, updated_at = ? WHERE subdomain = ?")
                .bind(err)
                .bind(now())
                .bind(env_id)
                .execute(&s.db)
                .await?;
        }
    }
    reload_routes(s).await;
    Ok(())
}

//! 平臺環境（FR-OPS-13，ADR 0015）：維運者在 Runner 開出、列出、刪除環境。
//!
//! 控制面配置子網域 ID、節點與網段索引，經 agent 連線送出 `deploy`／`destroy`；
//! agent 以 `env_status` 回報，running 的環境由 Edge 經通道轉發（M4-07）。
use super::admin::{get_node, new_env_id};
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::agent::{EnvService, EnvSpec, ServerMsg, STAGES};
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
    /// 專案的環境（FR-ENV-12）；維運者開的環境為 None
    pub project: Option<String>,
    /// 本次部署的目標 commit（ADR 0023）
    pub commit_sha: Option<String>,
    /// 健康檢查通過、正在執行的 commit 與上線時間（ADR 0023）
    pub running_commit: Option<String>,
    pub deployed_at: Option<i64>,
    /// 部署階段（ADR 0018）
    pub stage: Option<String>,
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
    project: Option<String>,
    commit_sha: Option<String>,
    running_commit: Option<String>,
    deployed_at: Option<i64>,
    stage: Option<String>,
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
            project: r.project,
            commit_sha: r.commit_sha,
            running_commit: r.running_commit,
            deployed_at: r.deployed_at,
            stage: r.stage,
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

const COLUMNS: &str = "subdomain, project, commit_sha, running_commit, deployed_at, stage, hostname, kind, node, subnet_index, status, error, public_service, public_port, services, created_by, created_at, updated_at";

pub(crate) async fn get(s: &AppState, subdomain: &str) -> ApiResult<Environment> {
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
pub(crate) async fn refresh_assigned(s: &AppState, node: &str) -> ApiResult<()> {
    sqlx::query(
        "UPDATE nodes SET assigned_cpu = (SELECT COALESCE(SUM(cpu_millis), 0) FROM environments WHERE node = ?1), \
         assigned_mem = (SELECT COALESCE(SUM(memory_bytes), 0) FROM environments WHERE node = ?1) WHERE id = ?1",
    )
    .bind(node)
    .execute(&s.db)
    .await?;
    Ok(())
}

/// 平臺節點中通過自我檢查者（FR-OPS-12）；可限定節點；`build` 時另需建置環境（ADR 0018）
async fn candidates(s: &AppState, only: Option<&str>, build: bool, gpu: bool) -> ApiResult<Vec<Candidate>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM nodes WHERE owner IS NULL ORDER BY id")
        .fetch_all(&s.db)
        .await?;
    let mut out = Vec::new();
    for (id,) in rows {
        if only.is_some_and(|o| o != id) {
            continue;
        }
        let n = get_node(s, &id).await?;
        if !n.specs.isolation || (build && !n.specs.buildkit) || (gpu && !n.specs.gpu_ready) || !s.agents.is_connected(&n.id) {
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

/// 調度保留量
pub(crate) fn need_for(services: usize) -> Resources {
    let n = services as u64;
    Resources {
        cpu_millis: PER_SERVICE.cpu_millis * n,
        memory_bytes: PER_SERVICE.memory_bytes * n,
    }
}

/// 節點的 GPU 是否就緒（ADR 0022）
pub(crate) async fn gpu_ready(s: &AppState, node: &str) -> ApiResult<bool> {
    Ok(get_node(s, node).await?.specs.gpu_ready)
}

/// 選節點並配置網段索引（M4-10）；`gpu`：只選 GPU 就緒的節點（ADR 0022）
pub(crate) async fn place(s: &AppState, need: Resources, only: Option<&str>, build: bool, gpu: bool) -> ApiResult<(String, i64)> {
    let nodes = candidates(s, only, build, gpu).await?;
    let node = choose_node(&nodes, need, Target::Platform)
        .ok_or_else(|| {
            ApiError::Conflict(if gpu {
                "沒有可用 GPU 的 Runner 節點：需要在線、通過自我檢查（含 GPU 檢查）且容量足夠的平臺節點。".into()
            } else if build {
                "沒有可用的 Runner 節點：需要在線、通過自我檢查、已安裝建置環境且容量足夠的平臺節點。".into()
            } else {
                "沒有可用的 Runner 節點：需要在線、通過自我檢查且容量足夠的平臺節點。".into()
            })
        })?
        .id
        .clone();
    let used: Vec<i64> = sqlx::query_scalar("SELECT subnet_index FROM environments WHERE node = ? ORDER BY subnet_index")
        .bind(&node)
        .fetch_all(&s.db)
        .await?;
    let index = (0..=i64::from(MAX_ALLOC_INDEX))
        .find(|i| used.binary_search(i).is_err())
        .ok_or_else(|| ApiError::Conflict("這個節點的環境網段已用完。".into()))?;
    Ok((node, index))
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
    let need = need_for(compose.services.len());
    let (node, index) = place(s, need, input.node_id.as_deref(), false, sandbox::needs_gpu(&compose)).await?;
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
            env: Box::new(EnvSpec {
                id: subdomain.clone(),
                kind: kind.as_str().into(),
                compose: input.compose_yaml.clone(),
                subnet_index: index as u16,
                public_service: input.public_service.clone(),
                source: None,
                build: None,
                release: vec![],
                vars: vec![],
                platform_vars: vec![],
                limits: vec![],
            }),
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
    destroy(s, &env).await?;
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

/// 請 agent 清除環境；節點離線時直接移除紀錄。專案關聯一併解除（專案可隨即刪除）
pub(crate) async fn destroy(s: &AppState, env: &Environment) -> ApiResult<()> {
    if s.agents.is_connected(&env.node) {
        sqlx::query("UPDATE environments SET status = 'destroying', project = NULL, updated_at = ? WHERE subdomain = ?")
            .bind(now())
            .bind(&env.subdomain)
            .execute(&s.db)
            .await?;
        s.agents.send(
            &env.node,
            ServerMsg::Destroy {
                env_id: env.subdomain.clone(),
            },
        );
    } else {
        remove(s, env).await?;
    }
    reload_routes(s).await;
    Ok(())
}

pub(crate) async fn remove(s: &AppState, env: &Environment) -> ApiResult<()> {
    sqlx::query("DELETE FROM deploy_logs WHERE env = ?")
        .bind(&env.subdomain)
        .execute(&s.db)
        .await?;
    sqlx::query("DELETE FROM environments WHERE subdomain = ?")
        .bind(&env.subdomain)
        .execute(&s.db)
        .await?;
    refresh_assigned(s, &env.node).await
}

pub(crate) async fn reload_routes(s: &AppState) {
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
    stage: Option<String>,
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
    let t = now();
    match status {
        "destroyed" => remove(s, &env).await?,
        // ADR 0018：階段變更；進入 start 時舊版停止，路由暫時移除
        "deploying" => {
            let Some(stage) = stage.filter(|x| STAGES.contains(&x.as_str())) else {
                return Ok(());
            };
            sqlx::query(
                "UPDATE environments SET stage = ?1, status = CASE WHEN ?1 = 'start' THEN 'deploying' ELSE status END, updated_at = ?2 \
                 WHERE subdomain = ?3 AND status IN ('deploying', 'running') AND error IS NULL",
            )
            .bind(&stage)
            .bind(t)
            .bind(env_id)
            .execute(&s.db)
            .await?;
            if stage != "start" {
                return Ok(());
            }
        }
        "running" => {
            let ip = services.iter().find(|x| x.name == env.public_service).map(|x| x.ip.clone());
            let svc = serde_json::to_string(&services).unwrap_or_else(|_| "[]".into());
            // 專案的環境先做健康檢查，通過後才 running（ADR 0018）
            let checked = env.project.is_some();
            let n = sqlx::query(
                "UPDATE environments SET status = ?, stage = CASE WHEN project IS NULL THEN stage ELSE 'health' END, error = NULL, \
                 public_ip = ?, services = ?, updated_at = ? WHERE subdomain = ? AND status = 'deploying'",
            )
            .bind(if checked { "deploying" } else { "running" })
            .bind(&ip)
            .bind(svc)
            .bind(t)
            .bind(env_id)
            .execute(&s.db)
            .await?
            .rows_affected();
            if checked && n > 0 {
                if let Some(ip) = ip {
                    super::deploy::spawn_health(s.clone(), env.clone(), format!("{ip}:{}", env.public_port));
                }
            }
        }
        _ => {
            let err: String = error.unwrap_or_else(|| "部署失敗".into()).chars().take(2000).collect();
            // 取得原始碼或建置失敗時，舊版仍在執行（ADR 0018）
            let keep = env.status == "running" && matches!(stage.as_deref(), Some("fetch" | "build" | "release"));
            sqlx::query("UPDATE environments SET status = ?, stage = COALESCE(?, stage), error = ?, updated_at = ? WHERE subdomain = ?")
                .bind(if keep { "running" } else { "failed" })
                .bind(&stage)
                .bind(&err)
                .bind(t)
                .bind(env_id)
                .execute(&s.db)
                .await?;
            // ADR 0023：部署紀錄與 GitHub Deployments
            if env.project.is_some() {
                super::deploy::finish(s, env_id, false, stage.as_deref().or(env.stage.as_deref()), Some(&err), keep).await;
            }
        }
    }
    reload_routes(s).await;
    Ok(())
}

/// 部署 log（ADR 0018 §3）：只接受該節點上的環境；每個環境保留最近 2000 行
pub(crate) async fn add_log(s: &AppState, node: Option<&str>, env_id: &str, stage: &str, line: &str) -> ApiResult<()> {
    if !STAGES.contains(&stage) {
        return Ok(());
    }
    let owner: Option<String> = sqlx::query_scalar("SELECT node FROM environments WHERE subdomain = ?")
        .bind(env_id)
        .fetch_optional(&s.db)
        .await?;
    if owner.is_none() || node.is_some_and(|n| owner.as_deref() != Some(n)) {
        return Ok(());
    }
    let line: String = line.chars().take(500).collect();
    let id = sqlx::query("INSERT INTO deploy_logs (env, stage, line, at) VALUES (?, ?, ?, ?)")
        .bind(env_id)
        .bind(stage)
        .bind(line)
        .bind(now())
        .execute(&s.db)
        .await?
        .last_insert_rowid();
    if id % 100 == 0 {
        sqlx::query("DELETE FROM deploy_logs WHERE env = ? AND id <= (SELECT id FROM deploy_logs WHERE env = ? ORDER BY id DESC LIMIT 1 OFFSET 2000)")
            .bind(env_id)
            .bind(env_id)
            .execute(&s.db)
            .await?;
    }
    Ok(())
}

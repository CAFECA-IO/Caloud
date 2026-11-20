//! 最小部署流程（FR-ENV-12，[ADR 0018](../../../../docs/adr/0018-minimal-deploy.md)）：
//! 確認後部署到 production 環境、回報階段與 log、健康檢查後開放網址。
//!
//! - 每個專案一個 production 環境：8 碼子網域，之後固定不變；
//! - 部署的 compose 是確認的版本；最新解析的 compose 與確認時相同，就部署最新的 commit，
//!   否則部署確認時的 commit，並提示「設定有變動，請重新確認」；
//! - 私有儲存庫附 installation token（只在 `Deploy` 訊息中）。
use super::environments::{self as envs, Environment};
use super::projects::owned;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_analyzer::Analysis;
use caloud_core::agent::{EnvSource, EnvSpec, ServerMsg};
use caloud_core::db::audit;
use caloud_core::model::{AnalysisRow, Project};
use caloud_core::now;
use caloud_core::subdomain::EnvKind;
use caloud_orchestrator::{validate_with, Policy};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 專案頁顯示的 log 行數
const SHOW_LOGS: i64 = 300;

fn conflict(m: impl Into<String>) -> ApiError {
    ApiError::Conflict(m.into())
}

async fn analysis(s: &AppState, id: &str) -> ApiResult<Option<AnalysisRow>> {
    Ok(sqlx::query_as("SELECT * FROM analyses WHERE id = ?")
        .bind(id)
        .fetch_optional(&s.db)
        .await?)
}

async fn latest(s: &AppState, project: &str) -> ApiResult<Option<AnalysisRow>> {
    Ok(
        sqlx::query_as("SELECT * FROM analyses WHERE project = ? ORDER BY created_at DESC, rowid DESC LIMIT 1")
            .bind(project)
            .fetch_optional(&s.db)
            .await?,
    )
}

fn parse(a: &AnalysisRow) -> ApiResult<Analysis> {
    serde_json::from_str(&a.result).map_err(|e| ApiError::Internal(e.into()))
}

/// 要部署（或驗證）的內容
pub(crate) struct Target {
    pub compose: String,
    pub compose_dir: String,
    pub public_service: String,
    pub public_port: i64,
    pub services: usize,
    pub commit: String,
    /// 最新解析的設定與確認時不同
    pub pending_changes: bool,
    /// 確認的解析 ID（部署關卡比對，ADR 0020）
    pub analysis: String,
    /// 驗證計畫（ADR 0020）
    pub verify: caloud_analyzer::VerifyPlan,
}

pub(crate) async fn target(s: &AppState, p: &Project) -> ApiResult<Target> {
    let cid = p
        .confirmed_analysis
        .as_deref()
        .ok_or_else(|| conflict("請先確認解析結果再部署。"))?;
    let c = analysis(s, cid).await?.ok_or_else(|| conflict("請先確認解析結果再部署。"))?;
    let ca = parse(&c)?;
    let compose = c
        .compose
        .clone()
        .or_else(|| ca.compose.clone())
        .ok_or_else(|| conflict("確認的解析沒有 compose。"))?;
    let public_service = c.public_service.clone().ok_or_else(|| conflict("請先選擇對外服務並確認。"))?;
    let depth = ca.compose_dir.split('/').filter(|x| !x.is_empty()).count();
    let v = validate_with(&compose, Policy::platform(depth)).map_err(|e| conflict(e.to_string()))?;
    let public_port = ca
        .services
        .iter()
        .find(|x| x.name == public_service)
        .and_then(|x| x.port)
        .ok_or_else(|| {
            conflict(format!(
                "對外服務 `{public_service}` 沒有偵測到連接埠，請在 compose 加上 expose 後重新解析。"
            ))
        })?;
    let (commit, pending_changes) = match latest(s, &p.id).await? {
        Some(l) if l.id != c.id => {
            let la = parse(&l)?;
            if la.compose == ca.compose && la.compose_dir == ca.compose_dir && !l.commit_sha.is_empty() {
                (l.commit_sha, false)
            } else {
                (c.commit_sha.clone(), true)
            }
        }
        _ => (c.commit_sha.clone(), false),
    };
    Ok(Target {
        analysis: c.id.clone(),
        verify: ca.verify.clone(),
        compose,
        compose_dir: ca.compose_dir,
        public_service,
        public_port: i64::from(public_port),
        services: v.services.len(),
        commit,
        pending_changes,
    })
}

pub(crate) async fn production(s: &AppState, project: &str) -> ApiResult<Option<Environment>> {
    let sub: Option<String> = sqlx::query_scalar("SELECT subdomain FROM environments WHERE project = ? AND kind = 'production'")
        .bind(project)
        .fetch_optional(&s.db)
        .await?;
    match sub {
        Some(x) => Ok(Some(envs::get(s, &x).await?)),
        None => Ok(None),
    }
}

/// 超過這個時間沒有進展，視為中斷（agent 重啟等），可以重新部署
const STALE_SECS: i64 = 40 * 60;

/// 部署進行中：在某個階段、尚未失敗且仍有進展
fn in_progress(e: &Environment) -> bool {
    e.error.is_none()
        && now() - e.updated_at < STALE_SECS
        && e.status != "failed"
        && matches!(e.stage.as_deref(), Some("fetch" | "build" | "start" | "health"))
}

/// `POST /api/projects/{id}/deploy` 的選項
#[derive(Debug, Default, serde::Deserialize)]
pub struct DeployInput {
    /// admin+：略過驗證（ADR 0020）
    #[serde(default)]
    pub force: bool,
}

/// `POST /api/projects/{id}/deploy`：目標 commit 未通過驗證時先驗證（ADR 0020）
pub async fn deploy(s: &AppState, me: &CurrentUser, id: &str, input: DeployInput) -> ApiResult<Value> {
    let p = owned(s, me, id).await?;
    if input.force {
        me.require_admin()?;
    } else {
        let t = target(s, &p).await?;
        if !super::verify::passed(s, &p.id, &t.commit, &t.analysis).await? {
            let v = super::verify::start(
                s,
                &p,
                "deploy",
                true,
                false,
                Some(&me.0.account),
                &me.audit_detail(None).unwrap_or_default(),
            )
            .await?;
            return Ok(json!({ "verification": v }));
        }
    }
    let env = start(s, &p).await?;
    let detail = format!(
        "{} {}{}",
        env.hostname,
        env.commit_sha.as_deref().unwrap_or_default().chars().take(12).collect::<String>(),
        if input.force { " forced" } else { "" }
    );
    audit(
        &s.db,
        Some(&me.0.account),
        "project_deploy",
        Some(id),
        me.audit_detail(Some(&detail)).as_deref(),
    )
    .await;
    Ok(deployment(s, &p).await?.unwrap_or(Value::Null))
}

/// 驗證通過後部署（`deploy_after`，ADR 0020）
pub(crate) async fn deploy_verified(s: &AppState, p: &Project, via: &str) {
    match start(s, p).await {
        Ok(env) => {
            audit(
                &s.db,
                None,
                "project_deploy",
                Some(&p.id),
                Some(&format!(
                    "via={via}; {} {} verified",
                    env.hostname,
                    env.commit_sha.as_deref().unwrap_or_default().chars().take(12).collect::<String>()
                )),
            )
            .await
        }
        Err(e) => tracing::warn!(project = %p.id, error = %e, "deploy after verification failed"),
    }
}

/// GitHub push 後：驗證；已部署且設定未變動時，通過後自動重新部署（ADR 0020）
pub(crate) async fn verify_after_push(s: &AppState, project: &str) {
    let Ok(p) = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(project)
        .fetch_one(&s.db)
        .await
    else {
        return;
    };
    if p.confirmed_analysis.is_none() {
        return;
    }
    let deployed = matches!(production(s, project).await, Ok(Some(_)));
    let unchanged = matches!(target(s, &p).await, Ok(t) if !t.pending_changes);
    if let Err(e) = super::verify::start(s, &p, "push", deployed && unchanged, false, None, "via=github").await {
        tracing::warn!(%project, error = %e, "verification after push skipped");
    }
}

pub(crate) async fn start(s: &AppState, p: &Project) -> ApiResult<Environment> {
    let t = target(s, p).await?;
    // ADR 0019：秘密變數（先讀版本，產生資料庫密碼不算變更）
    let vars_rev = super::vars::rev(s, &p.id).await?;
    let vars = super::vars::for_deploy(s, &p.id, "production", &t.compose).await?;
    let need = envs::need_for(t.services);
    let ts = now();
    let env = match production(s, &p.id).await? {
        Some(e) => {
            if in_progress(&e) || e.status == "destroying" {
                return Err(conflict("部署進行中，請等這次完成。"));
            }
            if !s.agents.is_connected(&e.node) {
                return Err(conflict("環境所在的 Runner 節點離線，請稍後再試。"));
            }
            sqlx::query(
                "UPDATE environments SET compose = ?, public_service = ?, public_port = ?, cpu_millis = ?, memory_bytes = ?, commit_sha = ?, \
                 stage = 'fetch', error = NULL, status = CASE WHEN status = 'running' THEN 'running' ELSE 'deploying' END, updated_at = ?, vars_rev = ? \
                 WHERE subdomain = ?",
            )
            .bind(&t.compose)
            .bind(&t.public_service)
            .bind(t.public_port)
            .bind(need.cpu_millis as i64)
            .bind(need.memory_bytes as i64)
            .bind(&t.commit)
            .bind(ts)
            .bind(vars_rev)
            .bind(&e.subdomain)
            .execute(&s.db)
            .await?;
            e
        }
        None => {
            let (node, index) = envs::place(s, need, None, true).await?;
            let (subdomain, hostname) = super::admin::new_env_id(s, EnvKind::Production).await?;
            sqlx::query(
                "INSERT INTO environments (subdomain, hostname, kind, node, subnet_index, compose, public_service, public_port, cpu_millis, memory_bytes, \
                 status, created_by, created_at, updated_at, project, commit_sha, stage, vars_rev) \
                 VALUES (?, ?, 'production', ?, ?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?, ?, ?, 'fetch', ?)",
            )
            .bind(&subdomain)
            .bind(&hostname)
            .bind(&node)
            .bind(index)
            .bind(&t.compose)
            .bind(&t.public_service)
            .bind(t.public_port)
            .bind(need.cpu_millis as i64)
            .bind(need.memory_bytes as i64)
            .bind(&p.owner)
            .bind(ts)
            .bind(ts)
            .bind(&p.id)
            .bind(&t.commit)
            .bind(vars_rev)
            .execute(&s.db)
            .await?;
            envs::get(s, &subdomain).await?
        }
    };
    envs::refresh_assigned(s, &env.node).await?;
    sqlx::query("DELETE FROM deploy_logs WHERE env = ?")
        .bind(&env.subdomain)
        .execute(&s.db)
        .await?;

    // ADR 0018 §3：installation token 只在 Deploy 訊息中
    let auth_header = match p.installation_id {
        Some(inst) => {
            let token = crate::github::app::installation_token(s, inst)
                .await
                .map_err(|e| conflict(format!("無法取得 GitHub 授權：{e}")))?;
            Some(crate::github::app::git_auth_header(&token))
        }
        None => None,
    };
    s.agents.send(
        &env.node,
        ServerMsg::Deploy {
            env: EnvSpec {
                id: env.subdomain.clone(),
                kind: "production".into(),
                compose: t.compose,
                subnet_index: env.subnet_index as u16,
                public_service: t.public_service,
                source: Some(EnvSource {
                    repo_url: p.repo_url.clone(),
                    commit: t.commit,
                    git_ref: p.git_ref.clone().or_else(|| p.branch.clone()),
                    compose_dir: t.compose_dir,
                    auth_header,
                }),
                vars,
            },
        },
    );
    envs::get(s, &env.subdomain).await
}

/// 專案頁的部署資訊（`GET /api/projects/{id}` 的 `deployment`）
pub(crate) async fn deployment(s: &AppState, p: &Project) -> ApiResult<Option<Value>> {
    let Some(e) = production(s, &p.id).await? else {
        return Ok(None);
    };
    let mut logs: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT stage, line, at FROM deploy_logs WHERE env = ? ORDER BY id DESC LIMIT ?")
            .bind(&e.subdomain)
            .bind(SHOW_LOGS)
            .fetch_all(&s.db)
            .await?;
    logs.reverse();
    let env_rev: Option<i64> = sqlx::query_scalar("SELECT vars_rev FROM environments WHERE subdomain = ?")
        .bind(&e.subdomain)
        .fetch_one(&s.db)
        .await?;
    let vars_pending = env_rev.unwrap_or(0) != super::vars::rev(s, &p.id).await?;
    let missing_vars = super::vars::missing(s, &p.id, "production").await?;
    let pending = match target(s, p).await {
        Ok(t) => t.pending_changes,
        Err(_) => false,
    };
    Ok(Some(json!({
        "subdomain": e.subdomain,
        "hostname": e.hostname,
        "url": format!("https://{}", e.hostname),
        "status": e.status,
        "stage": e.stage,
        "error": e.error,
        "commit": e.commit_sha,
        "node": e.node,
        "updated_at": e.updated_at,
        "in_progress": in_progress(&e),
        "pending_changes": pending,
        "vars_pending": vars_pending,
        "missing_vars": missing_vars,
        "logs": logs.into_iter().map(|(stage, line, at)| json!({ "stage": stage, "line": line, "at": at })).collect::<Vec<_>>(),
    })))
}

/// 經通道對 `target` 發一次 `GET /`；收到任何 HTTP 回應即通過
pub async fn probe(tunnels: &caloud_proxy::TunnelRegistry, node: Option<&str>, target: &str, host: &str) -> bool {
    let job = async {
        let mut c = tunnels.dial(node, target).await.ok()?;
        c.write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Caloud-Health\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .ok()?;
        let mut buf = [0u8; 5];
        c.read_exact(&mut buf).await.ok()?;
        Some(&buf == b"HTTP/")
    };
    tokio::time::timeout(Duration::from_secs(10), job)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// 健康檢查（ADR 0018 §1.5）：通過後 running 並開放路由，逾時則 failed
pub(crate) fn spawn_health(s: AppState, env: Environment, target: String) {
    tokio::spawn(async move {
        let secs = s.cfg.deploy_health_secs;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        let log = |line: String| {
            let s = s.clone();
            let id = env.subdomain.clone();
            async move {
                let _ = envs::add_log(&s, None, &id, "health", &line).await;
            }
        };
        let ok = if secs == 0 {
            true
        } else {
            log(format!("等待 {} 回應 HTTP（最多 {secs} 秒）", env.public_service)).await;
            loop {
                if probe(&s.tunnels, Some(&env.node), &target, &env.hostname).await {
                    break true;
                }
                if tokio::time::Instant::now() >= deadline {
                    break false;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        };
        let res = if ok {
            log("健康檢查通過，網址已開放".into()).await;
            sqlx::query(
                "UPDATE environments SET status = 'running', stage = 'done', updated_at = ? WHERE subdomain = ? AND stage = 'health' AND status = 'deploying'",
            )
            .bind(now())
            .bind(&env.subdomain)
            .execute(&s.db)
            .await
        } else {
            let msg = format!(
                "{secs} 秒內服務 `{}` 沒有在 port {} 回應 HTTP，請確認服務有監聽 0.0.0.0 與正確的連接埠。",
                env.public_service, env.public_port
            );
            log(msg.clone()).await;
            sqlx::query(
                "UPDATE environments SET status = 'failed', error = ?, updated_at = ? WHERE subdomain = ? AND stage = 'health' AND status = 'deploying'",
            )
            .bind(msg)
            .bind(now())
            .bind(&env.subdomain)
            .execute(&s.db)
            .await
        };
        if let Err(e) = res {
            tracing::error!(error = %e, "health result not saved");
        }
        envs::reload_routes(&s).await;
    });
}

#[cfg(test)]
mod tests {
    use super::probe;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ADR 0018：任何 HTTP 回應即通過；非 HTTP 或連不上則不通過
    #[tokio::test]
    async fn probe_accepts_any_http_response() {
        let serve = |reply: &'static [u8]| async move {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            tokio::spawn(async move {
                while let Ok((mut c, _)) = l.accept().await {
                    let mut buf = [0u8; 256];
                    let n = c.read(&mut buf).await.unwrap_or(0);
                    assert!(String::from_utf8_lossy(&buf[..n]).contains("Host: abc.caloud.tw"));
                    let _ = c.write_all(reply).await;
                }
            });
            addr.to_string()
        };
        let t = caloud_proxy::TunnelRegistry::default();
        let a = serve(b"HTTP/1.1 500 Internal Server Error\r\n\r\n").await;
        assert!(probe(&t, None, &a, "abc.caloud.tw").await);
        let b = serve(b"SSH-2.0-OpenSSH\r\n").await;
        assert!(!probe(&t, None, &b, "abc.caloud.tw").await);
        assert!(!probe(&t, None, "127.0.0.1:1", "abc.caloud.tw").await);
        assert!(!probe(&t, Some("offline-node"), &a, "abc.caloud.tw").await);
    }
}

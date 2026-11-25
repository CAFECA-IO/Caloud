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
use caloud_core::agent::{EnvSource, EnvSpec, PlatformVar, ServerMsg};
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
    pub commit: String,
    /// 最新解析的設定與確認時不同
    pub pending_changes: bool,
    /// 確認的解析 ID（ADR 0020）
    pub analysis: String,
    /// 設定指紋（部署關卡與自動部署比對，ADR 0024）
    pub settings: String,
    /// 驗證計畫（ADR 0020）
    pub verify: caloud_analyzer::VerifyPlan,
    /// 有服務宣告 GPU（ADR 0022）
    pub gpu: bool,
    /// 各服務的資源上限與排程預留量（ADR 0025）
    pub limits: Vec<caloud_core::agent::ServiceLimit>,
    pub need: caloud_core::placement::Resources,
}

/// 設定指紋（ADR 0024）：有效 compose（編輯過的優先）、compose 目錄、對外服務。
/// 部署與驗證只依這三項建置與啟動；指紋相同即同一份設定。
pub(crate) fn settings_key(a: &AnalysisRow) -> Option<String> {
    let r: Analysis = serde_json::from_str(&a.result).ok()?;
    let compose = a.compose.clone().or(r.compose)?;
    let raw = format!("{compose}\0{}\0{}", r.compose_dir, a.public_service.as_deref().unwrap_or_default());
    Some(hex::encode(
        &ring::digest::digest(&ring::digest::SHA256, raw.as_bytes()).as_ref()[..16],
    ))
}

/// 改動一筆解析的確認內容前，先把引用它、還沒有記錄設定指紋的驗證固定下來（ADR 0024）
pub(crate) async fn freeze_settings(s: &AppState, a: &AnalysisRow) -> ApiResult<()> {
    if let Some(key) = settings_key(a) {
        sqlx::query("UPDATE verifications SET settings = ? WHERE analysis = ? AND settings IS NULL")
            .bind(key)
            .bind(&a.id)
            .execute(&s.db)
            .await?;
    }
    Ok(())
}

/// 確認的解析
pub(crate) async fn confirmed(s: &AppState, p: &Project) -> ApiResult<AnalysisRow> {
    let cid = p
        .confirmed_analysis
        .as_deref()
        .ok_or_else(|| conflict("請先確認解析結果再部署。"))?;
    analysis(s, cid).await?.ok_or_else(|| conflict("請先確認解析結果再部署。"))
}

/// 最新解析產生的設定與確認時相同（compose 與 compose 目錄）
fn same_generated(a: &Analysis, b: &Analysis) -> bool {
    a.compose.is_some() && a.compose == b.compose && a.compose_dir == b.compose_dir
}

pub(crate) async fn target(s: &AppState, p: &Project) -> ApiResult<Target> {
    let c = confirmed(s, p).await?;
    let ca = parse(&c)?;
    let (commit, pending_changes) = match latest(s, &p.id).await? {
        Some(l) if l.id != c.id => {
            if same_generated(&parse(&l)?, &ca) && !l.commit_sha.is_empty() {
                (l.commit_sha, false)
            } else {
                (c.commit_sha.clone(), true)
            }
        }
        _ => (c.commit_sha.clone(), false),
    };
    target_for(p, &c, commit, pending_changes)
}

/// 確認的有效 compose（通過平臺政策）；沒有確認時為 None
pub(crate) async fn confirmed_compose(s: &AppState, p: &Project) -> ApiResult<Option<caloud_orchestrator::ValidatedCompose>> {
    let Some(cid) = p.confirmed_analysis.as_deref() else {
        return Ok(None);
    };
    let Some(c) = analysis(s, cid).await? else { return Ok(None) };
    let ca = parse(&c)?;
    let depth = ca.compose_dir.split('/').filter(|x| !x.is_empty()).count();
    Ok(c.compose
        .clone()
        .or(ca.compose)
        .and_then(|y| validate_with(&y, Policy::platform(depth)).ok()))
}

/// 以一筆（已確認的）解析與指定 commit 組成部署內容（ADR 0024：部署指定版本）
pub(crate) fn target_for(p: &Project, c: &AnalysisRow, commit: String, pending_changes: bool) -> ApiResult<Target> {
    let ca = parse(c)?;
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
    // ADR 0025：依專案上限決定各服務資源
    let resolved = super::limits::resolve(&v, p.memory_limit_mb, p.cpu_limit).map_err(conflict)?;
    Ok(Target {
        need: super::limits::need(&resolved),
        limits: resolved.into_iter().map(|r| r.limit).collect(),
        analysis: c.id.clone(),
        settings: settings_key(c).unwrap_or_default(),
        verify: ca.verify.clone(),
        compose,
        compose_dir: ca.compose_dir,
        public_service,
        public_port: i64::from(public_port),
        gpu: caloud_orchestrator::sandbox::needs_gpu(&v),
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
        && matches!(e.stage.as_deref(), Some("fetch" | "build" | "release" | "start" | "health"))
}

/// 平臺變數（ADR 0023）：App 可讀的版本資訊
pub(crate) fn platform_vars(commit: &str, branch: &str, kind: &str) -> Vec<PlatformVar> {
    let at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    [
        ("CALOUD_COMMIT", commit.to_string()),
        ("CALOUD_COMMIT_SHORT", commit.chars().take(7).collect()),
        ("CALOUD_BRANCH", branch.to_string()),
        ("CALOUD_ENV", kind.to_string()),
        ("CALOUD_DEPLOYED_AT", at),
    ]
    .into_iter()
    .map(|(n, v)| PlatformVar { name: n.into(), value: v })
    .collect()
}

/// 平臺映像 `caloud-env/<env>/<service>:<commit 前 12 碼>` 的 commit；現成映像為 None
pub(crate) fn image_commit(image: &str) -> Option<String> {
    let rest = image.strip_prefix("caloud-env/")?;
    let (_, tag) = rest.rsplit_once(':')?;
    (!tag.is_empty() && tag.len() <= 40 && tag.bytes().all(|b| b.is_ascii_hexdigit())).then(|| tag.to_string())
}

/// `POST /api/projects/{id}/deploy` 的選項
#[derive(Debug, Default, serde::Deserialize)]
pub struct DeployInput {
    /// admin+：略過驗證（ADR 0020）
    #[serde(default)]
    pub force: bool,
    /// 部署指定版本（ADR 0024）：已通過驗證或曾成功上線的 commit
    #[serde(default)]
    pub commit: Option<String>,
}

/// 指定版本使用的設定（ADR 0024）：最近一次通過驗證所用的解析；沒有時用曾成功部署時的解析。
/// 回傳（解析、是否為回到舊版）
async fn analysis_for_commit(s: &AppState, project: &str, commit: &str) -> ApiResult<(AnalysisRow, bool)> {
    let verified: Option<String> = sqlx::query_scalar(
        "SELECT analysis FROM verifications WHERE project = ? AND commit_sha = ? AND status = 'passed' AND trigger != 'pr' \
         ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(project)
    .bind(commit)
    .fetch_optional(&s.db)
    .await?;
    if let Some(a) = verified {
        if let Some(row) = analysis(s, &a).await? {
            return Ok((row, false));
        }
    }
    let deployed: Option<String> = sqlx::query_scalar(
        "SELECT analysis FROM deployments WHERE project = ? AND commit_sha = ? AND status = 'success' AND analysis IS NOT NULL \
         ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(project)
    .bind(commit)
    .fetch_optional(&s.db)
    .await?;
    if let Some(a) = deployed {
        if let Some(row) = analysis(s, &a).await? {
            return Ok((row, true));
        }
    }
    Err(conflict("這個版本沒有通過驗證，也沒有成功上線過，不能直接部署。"))
}

/// `POST /api/projects/{id}/deploy`：目標 commit 未通過驗證時先驗證（ADR 0020）；
/// 帶 `commit` 時部署指定版本（ADR 0024）
pub async fn deploy(s: &AppState, me: &CurrentUser, id: &str, input: DeployInput) -> ApiResult<Value> {
    let p = owned(s, me, id).await?;
    if let Some(commit) = input.commit.as_deref() {
        if input.force {
            return Err(ApiError::BadRequest("指定版本時不能同時強制部署。".into()));
        }
        if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ApiError::BadRequest("commit 需為 40 碼的 sha。".into()));
        }
        let (row, rollback) = analysis_for_commit(s, &p.id, commit).await?;
        let t = target_for(&p, &row, commit.to_string(), false)?;
        let env = start_target(s, &p, t, "manual", Some(&me.0.account)).await?;
        let detail = format!(
            "{} {}{}",
            env.hostname,
            &commit[..12],
            if rollback { " rollback" } else { " pinned" }
        );
        audit(
            &s.db,
            Some(&me.0.account),
            "project_deploy",
            Some(id),
            me.audit_detail(Some(&detail)).as_deref(),
        )
        .await;
        return Ok(deployment(s, &p).await?.unwrap_or(Value::Null));
    }
    if input.force {
        me.require_admin()?;
    } else {
        let t = target(s, &p).await?;
        if !super::verify::passed(s, &p.id, &t.commit, &t.settings).await? {
            let v = super::verify::start(
                s,
                &p,
                "deploy",
                true,
                false,
                Some(&me.0.account),
                &me.audit_detail(None).unwrap_or_default(),
                None,
            )
            .await?;
            return Ok(json!({ "verification": v }));
        }
    }
    let env = start(s, &p, if input.force { "forced" } else { "manual" }, Some(&me.0.account)).await?;
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

/// 驗證通過後部署（`deploy_after`，ADR 0020）：部署通過驗證的那個 commit；`by` 為觸發驗證的人
pub(crate) async fn deploy_verified(s: &AppState, p: &Project, commit: &str, via: &str, by: Option<&str>) -> ApiResult<()> {
    let trigger = if via == "github" { "push" } else { "manual" };
    let c = confirmed(s, p).await?;
    let t = target_for(p, &c, commit.to_string(), false)?;
    let env = start_target(s, p, t, trigger, by).await?;
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
    .await;
    Ok(())
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
    if let Err(e) = super::verify::start(s, &p, "push", deployed && unchanged, false, None, "via=github", None).await {
        tracing::warn!(%project, error = %e, "verification after push skipped");
    }
}

/// 以確認的設定開始部署；`trigger`：`manual`、`forced`、`push`（ADR 0023 部署紀錄）
pub(crate) async fn start(s: &AppState, p: &Project, trigger: &str, by: Option<&str>) -> ApiResult<Environment> {
    let t = target(s, p).await?;
    start_target(s, p, t, trigger, by).await
}

/// 以指定的部署內容開始部署
pub(crate) async fn start_target(s: &AppState, p: &Project, t: Target, trigger: &str, by: Option<&str>) -> ApiResult<Environment> {
    // ADR 0021：建置分鐘上限與建置資源
    super::build::check_quota(s, &p.owner).await?;
    let build = super::build::limits(s).await?;
    // ADR 0019：秘密變數（先讀版本，產生資料庫密碼不算變更）
    let vars_rev = super::vars::rev(s, &p.id).await?;
    let vars = super::vars::for_deploy(s, &p.id, "production", &t.compose).await?;
    let need = t.need;
    let ts = now();
    let env = match production(s, &p.id).await? {
        Some(e) => {
            if in_progress(&e) || e.status == "destroying" {
                return Err(conflict("部署進行中，請等這次完成。"));
            }
            if !s.agents.is_connected(&e.node) {
                return Err(conflict("環境所在的 Runner 節點離線，請稍後再試。"));
            }
            if t.gpu && !envs::gpu_ready(s, &e.node).await? {
                return Err(conflict("服務宣告了 GPU，但這個環境所在的 Runner 節點沒有可用的 GPU。"));
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
            let (node, index) = envs::place(s, need, None, true, t.gpu).await?;
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

    // ADR 0023：部署紀錄；先前未結束的（agent 中斷等）標為失敗
    sqlx::query(
        "UPDATE deployments SET status = 'failure', error = '被新的部署取代', finished_at = ? WHERE env = ? AND status = 'in_progress'",
    )
    .bind(ts)
    .bind(&env.subdomain)
    .execute(&s.db)
    .await?;
    let dep_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO deployments (id, project, env, commit_sha, trigger, status, created_by, created_at, analysis) VALUES (?, ?, ?, ?, ?, 'in_progress', ?, ?, ?)",
    )
    .bind(&dep_id)
    .bind(&p.id)
    .bind(&env.subdomain)
    .bind(&t.commit)
    .bind(trigger)
    .bind(by)
    .bind(ts)
    .bind(&t.analysis)
    .execute(&s.db)
    .await?;
    let env_url = format!("https://{}", env.hostname);
    crate::github::deployments::spawn_create(s.clone(), p.clone(), dep_id, t.commit.clone(), env_url);
    let branch = p.git_ref.clone().or_else(|| p.branch.clone()).unwrap_or_default();
    let platform_vars = platform_vars(&t.commit, &branch, "production");

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
            env: Box::new(EnvSpec {
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
                build: Some(build),
                release: t
                    .verify
                    .release
                    .iter()
                    .map(|r| caloud_core::agent::ReleaseTask {
                        service: r.service.clone(),
                        command: r.command.clone(),
                    })
                    .collect(),
                vars,
                platform_vars,
                limits: t.limits.clone(),
            }),
        },
    );
    envs::get(s, &env.subdomain).await
}

/// 部署結束（ADR 0023）：更新部署紀錄並回寫 GitHub Deployments。
/// `kept_old`：失敗時舊版仍在執行（取得原始碼、建置、發布前任務）
pub(crate) async fn finish(s: &AppState, env_id: &str, ok: bool, stage: Option<&str>, error: Option<&str>, kept_old: bool) {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, project, commit_sha FROM deployments WHERE env = ? AND status = 'in_progress' ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(env_id)
    .fetch_optional(&s.db)
    .await
    .ok()
    .flatten();
    let Some((dep_id, project, commit)) = row else { return };
    let error: Option<String> = error.map(|e| e.chars().take(2000).collect());
    if let Err(e) = sqlx::query("UPDATE deployments SET status = ?, stage = ?, error = ?, finished_at = ? WHERE id = ?")
        .bind(if ok { "success" } else { "failure" })
        .bind(if ok { None } else { stage })
        .bind(&error)
        .bind(now())
        .bind(&dep_id)
        .execute(&s.db)
        .await
    {
        tracing::error!(error = %e, "deployment record not saved");
    }
    let Ok(p) = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&project)
        .fetch_one(&s.db)
        .await
    else {
        return;
    };
    let hostname: Option<String> = sqlx::query_scalar("SELECT hostname FROM environments WHERE subdomain = ?")
        .bind(env_id)
        .fetch_optional(&s.db)
        .await
        .ok()
        .flatten();
    let short: String = commit.chars().take(7).collect();
    let description = if ok {
        format!("已上線 {short}")
    } else {
        format!(
            "{} 階段失敗{}：{}",
            stage.unwrap_or("部署"),
            if kept_old { "，仍執行舊版" } else { "" },
            error.as_deref().unwrap_or("未知錯誤").lines().next().unwrap_or_default()
        )
    };
    let url = hostname.map(|h| format!("https://{h}")).unwrap_or_default();
    crate::github::deployments::spawn_finish(s.clone(), p, dep_id, ok, description, url);
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
    // ADR 0023：實際映像、分支最新 commit 與它的驗證狀態
    let services: Vec<Value> = e
        .services
        .iter()
        .map(|x| json!({ "name": x.name, "image": x.image, "commit": x.image.as_deref().and_then(image_commit) }))
        .collect();
    let branch_commit: Option<String> = latest(s, &p.id).await?.map(|a| a.commit_sha);
    let branch_verify: Option<String> = match &branch_commit {
        Some(c) => sqlx::query_scalar(
            "SELECT status FROM verifications WHERE project = ? AND commit_sha = ? AND trigger != 'pr' ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )
        .bind(&p.id)
        .bind(c)
        .fetch_optional(&s.db)
        .await?,
        None => None,
    };
    Ok(Some(json!({
        "subdomain": e.subdomain,
        "hostname": e.hostname,
        "url": format!("https://{}", e.hostname),
        "status": e.status,
        "stage": e.stage,
        "error": e.error,
        "commit": e.commit_sha,
        "target_commit": e.commit_sha,
        "running_commit": e.running_commit,
        "deployed_at": e.deployed_at,
        "services": services,
        "branch_commit": branch_commit,
        "branch_verify": branch_verify,
        "node": e.node,
        "updated_at": e.updated_at,
        "in_progress": in_progress(&e),
        "pending_changes": pending,
        "vars_pending": vars_pending,
        "missing_vars": missing_vars,
        "logs": logs.into_iter().map(|(stage, line, at)| json!({ "stage": stage, "line": line, "at": at })).collect::<Vec<_>>(),
    })))
}

/// 部署紀錄（ADR 0023）
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct DeploymentRecord {
    pub id: String,
    pub env: String,
    #[serde(rename = "commit")]
    pub commit_sha: String,
    pub trigger: String,
    pub status: String,
    pub stage: Option<String>,
    pub error: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    /// 可「回到這個版本」（ADR 0024）
    pub rollback: bool,
}

/// `GET /api/projects/{id}/deployments`：最近 20 筆
pub async fn history(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Vec<DeploymentRecord>> {
    owned(s, me, id).await?;
    let running: Option<String> = production(s, id).await?.and_then(|e| e.running_commit);
    Ok(sqlx::query_as(
        "SELECT d.id, d.env, d.commit_sha, d.trigger, d.status, d.stage, d.error, d.created_by, d.created_at, d.finished_at, \
         (d.status = 'success' AND d.commit_sha != COALESCE(?, '') AND (d.analysis IS NOT NULL OR EXISTS ( \
            SELECT 1 FROM verifications v WHERE v.project = d.project AND v.commit_sha = d.commit_sha AND v.status = 'passed' AND v.trigger != 'pr'))) AS rollback \
         FROM deployments d WHERE d.project = ? ORDER BY d.created_at DESC, d.rowid DESC LIMIT 20",
    )
    .bind(running)
    .bind(id)
    .fetch_all(&s.db)
    .await?)
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
            // ADR 0023：執行中的版本只在這裡更新
            sqlx::query(
                "UPDATE environments SET status = 'running', stage = 'done', running_commit = commit_sha, deployed_at = ?1, updated_at = ?1 \
                 WHERE subdomain = ?2 AND stage = 'health' AND status = 'deploying'",
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
            .bind(&msg)
            .bind(now())
            .bind(&env.subdomain)
            .execute(&s.db)
            .await
        };
        match res {
            Ok(r) if r.rows_affected() > 0 => {
                let err = (!ok).then(|| format!("{secs} 秒內服務 `{}` 沒有回應 HTTP", env.public_service));
                finish(&s, &env.subdomain, ok, Some("health"), err.as_deref(), false).await;
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "health result not saved"),
        }
        envs::reload_routes(&s).await;
    });
}

#[cfg(test)]
mod tests {
    use super::{image_commit, platform_vars, probe};

    // ADR 0023
    #[test]
    fn reads_commit_from_platform_images_and_builds_platform_vars() {
        assert_eq!(image_commit("caloud-env/k2m9/web:98b705d01234").as_deref(), Some("98b705d01234"));
        assert_eq!(image_commit("postgres:16"), None);
        assert_eq!(image_commit("caloud-env/k2m9/web:latest"), None);
        assert_eq!(image_commit("caloud-env/k2m9/web"), None);
        let v = platform_vars("98b705d0123456789abcdef0123456789abcdef0", "main", "production");
        let get = |n: &str| v.iter().find(|x| x.name == n).map(|x| x.value.clone()).unwrap();
        assert_eq!(get("CALOUD_COMMIT_SHORT"), "98b705d");
        assert_eq!((get("CALOUD_BRANCH").as_str(), get("CALOUD_ENV").as_str()), ("main", "production"));
        assert!(get("CALOUD_DEPLOYED_AT").ends_with('Z'));
    }
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

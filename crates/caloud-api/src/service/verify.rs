//! 驗證與報告（M6，[ADR 0020](../../../../docs/adr/0020-verification.md)）。
//!
//! - `start`：配置 verify 環境（16 字元、不進路由），取消同分支的舊執行，送 `Verify` 給 agent；
//! - agent 回報階段、問題、log 與結束；控制面負責 `analyze` 與 `report`：去重、判定、與前次比較；
//! - 通過且 `deploy_after` 時接著部署；失敗且要求保留時 30 分鐘後銷毀；
//! - 背景巡檢：逾時或節點離線判為 error、到期的保留環境、每個專案保留最近 50 次。
use super::deploy;
use super::environments as envs;
use super::projects::owned;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::agent::{EnvSource, EnvSpec, Issue, ReleaseTask, ServerMsg, VerifyCmd, VerifySpec, SEVERITIES, VERIFY_STAGES};
use caloud_core::db::audit;
use caloud_core::model::Project;
use caloud_core::now;
use caloud_core::subdomain::EnvKind;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::time::Duration;

/// 整體逾時（ADR 0020）
pub const RUN_TIMEOUT: i64 = 45 * 60;
/// 失敗時保留環境的時間（FR-CI-05）
pub const KEEP_SECS: i64 = 30 * 60;
/// 每個專案保留的執行數
pub const KEEP_RUNS: i64 = 50;
/// 每次執行保留的 log 行數
const MAX_LOGS: i64 = 2000;
/// 報告與專案頁顯示的 log 行數
const SHOW_LOGS: i64 = 500;

fn conflict(m: impl Into<String>) -> ApiError {
    ApiError::Conflict(m.into())
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Verification {
    pub id: String,
    pub project: String,
    #[serde(skip)]
    pub analysis: String,
    #[serde(rename = "commit")]
    pub commit_sha: String,
    pub branch: String,
    pub trigger: String,
    pub status: String,
    pub stage: Option<String>,
    #[serde(serialize_with = "as_json")]
    pub stages: String,
    #[serde(serialize_with = "as_json")]
    pub counts: String,
    #[serde(skip)]
    pub env: Option<String>,
    #[serde(skip)]
    pub node: Option<String>,
    pub deploy_after: bool,
    pub keep: bool,
    pub keep_until: Option<i64>,
    pub error: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    #[serde(skip)]
    pub check_run_id: Option<i64>,
    pub pr_number: Option<i64>,
    pub build_secs: i64,
}

fn as_json<S: serde::Serializer>(v: &str, s: S) -> Result<S::Ok, S::Error> {
    serde_json::from_str::<Value>(v).unwrap_or(Value::Null).serialize(s)
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct IssueRow {
    pub fingerprint: String,
    pub severity: String,
    pub stage: String,
    pub rule: String,
    pub title: String,
    pub detail: String,
    pub file: Option<String>,
    pub line: Option<i64>,
}

fn finished(status: &str) -> bool {
    matches!(status, "passed" | "failed" | "canceled" | "error")
}

/// 指紋：`sha256(stage|rule|file|正規化 title)` 前 16 碼（FR-RPT-01）
pub fn fingerprint(i: &Issue) -> String {
    let title: String = i
        .title
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let raw = format!("{}|{}|{}|{}", i.stage, i.rule, i.file.as_deref().unwrap_or_default(), title);
    let d = ring::digest::digest(&ring::digest::SHA256, raw.as_bytes());
    hex::encode(&d.as_ref()[..8])
}

/// 有 Blocker 或 High 即失敗
pub fn verdict(counts: &Value) -> &'static str {
    let n = |k: &str| counts[k].as_i64().unwrap_or(0);
    if n("blocker") + n("high") > 0 {
        "failed"
    } else {
        "passed"
    }
}

async fn get(s: &AppState, id: &str) -> ApiResult<Verification> {
    sqlx::query_as("SELECT * FROM verifications WHERE id = ?")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// 驗證的專案必須屬於自己（或 admin+）
async fn owned_run(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Verification> {
    let v = get(s, id).await?;
    owned(s, me, &v.project).await?;
    Ok(v)
}

/// 部署關卡：這個 commit 在這份確認的設定下已通過驗證
pub(crate) async fn passed(s: &AppState, project: &str, commit: &str, analysis: &str) -> ApiResult<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM verifications WHERE project = ? AND commit_sha = ? AND analysis = ? AND status = 'passed' AND trigger != 'pr'",
    )
    .bind(project)
    .bind(commit)
    .bind(analysis)
    .fetch_one(&s.db)
    .await?;
    Ok(n > 0)
}

/// `POST /api/projects/{id}/verify`
pub async fn manual(s: &AppState, me: &CurrentUser, id: &str, keep: bool) -> ApiResult<Verification> {
    let p = owned(s, me, id).await?;
    start(
        s,
        &p,
        "manual",
        false,
        keep,
        Some(&me.0.account),
        &me.audit_detail(None).unwrap_or_default(),
        None,
    )
    .await
}

/// 取消進行中的執行（不寫狀態以外的資料；agent 結束時釋放環境）
async fn cancel_run(s: &AppState, v: &Verification, why: &str) -> ApiResult<()> {
    let n = sqlx::query(
        "UPDATE verifications SET status = 'canceled', error = ?, finished_at = ? WHERE id = ? AND status IN ('queued', 'running')",
    )
    .bind(why)
    .bind(now())
    .bind(&v.id)
    .execute(&s.db)
    .await?
    .rows_affected();
    if n > 0 {
        notify_github(s, &v.id).await;
        if let Some(node) = &v.node {
            if s.agents.is_connected(node) {
                s.agents.send(node, ServerMsg::CancelVerify { id: v.id.clone() });
            } else {
                release_env(s, v, false).await?;
            }
        }
    }
    Ok(())
}

/// 開始一次驗證；`via`：稽核來源（`via=web`、`via=github`…）
#[allow(clippy::too_many_arguments)] // 觸發來源各自需要的參數；PR 另帶 (number, head)
pub(crate) async fn start(
    s: &AppState,
    p: &Project,
    trigger: &str,
    deploy_after: bool,
    keep: bool,
    actor: Option<&str>,
    via: &str,
    pr: Option<(i64, String)>,
) -> ApiResult<Verification> {
    let mut t = deploy::target(s, p).await.map_err(|e| match e {
        ApiError::Conflict(m) => ApiError::Conflict(m.replace("部署", "驗證")),
        other => other,
    })?;
    // ADR 0021：建置分鐘上限
    super::build::check_quota(s, &p.owner).await?;
    let build = super::build::limits(s).await?;
    // PR 驗證：以確認的設定驗證 head commit；不部署、不注入用戶變數
    let (branch, git_ref, pr_number) = match &pr {
        Some((n, head)) => {
            t.commit = head.clone();
            t.pending_changes = false;
            (format!("pr/{n}"), Some(format!("refs/pull/{n}/head")), Some(*n))
        }
        None => (
            p.git_ref.clone().or_else(|| p.branch.clone()).unwrap_or_else(|| "HEAD".into()),
            p.git_ref.clone().or_else(|| p.branch.clone()),
            None,
        ),
    };
    let deploy_after = deploy_after && pr.is_none();
    // FR-CI-04：同分支的舊執行取消
    let running: Vec<Verification> =
        sqlx::query_as("SELECT * FROM verifications WHERE project = ? AND branch = ? AND status IN ('queued', 'running')")
            .bind(&p.id)
            .bind(&branch)
            .fetch_all(&s.db)
            .await?;
    for v in &running {
        cancel_run(s, v, "同分支有新的驗證").await?;
    }

    let need = envs::need_for(t.services);
    let (node, index) = envs::place(s, need, None, true).await?;
    let (subdomain, hostname) = super::admin::new_env_id(s, EnvKind::Verify).await?;
    let vars = super::vars::for_verify(s, &p.id, &t.compose, pr.is_none()).await?;
    let auth_header = match p.installation_id {
        Some(inst) => {
            let token = crate::github::app::installation_token(s, inst)
                .await
                .map_err(|e| conflict(format!("無法取得 GitHub 授權：{e}")))?;
            Some(crate::github::app::git_auth_header(&token))
        }
        None => None,
    };
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let mut stages = Map::new();
    for st in VERIFY_STAGES {
        stages.insert(st.into(), "pending".into());
    }
    stages.insert("analyze".into(), "passed".into());
    if t.pending_changes {
        stages.insert("analyze".into(), "failed".into());
    }
    let owner = actor.map(str::to_string).unwrap_or_else(|| p.owner.clone());
    let mut tx = s.db.begin().await?;
    // 驗證環境：不屬於專案（不受 production 唯一限制）、不進路由
    sqlx::query(
        "INSERT INTO environments (subdomain, hostname, kind, node, subnet_index, compose, public_service, public_port, cpu_millis, memory_bytes, \
         status, created_by, created_at, updated_at, commit_sha) VALUES (?, ?, 'verify', ?, ?, ?, ?, ?, ?, ?, 'deploying', ?, ?, ?, ?)",
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
    .bind(&owner)
    .bind(ts)
    .bind(ts)
    .bind(&t.commit)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO verifications (id, project, analysis, commit_sha, branch, trigger, status, stage, stages, counts, env, node, deploy_after, keep, created_by, created_at, started_at, pr_number) \
         VALUES (?, ?, ?, ?, ?, ?, 'running', 'fetch', ?, '{}', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&p.id)
    .bind(&t.analysis)
    .bind(&t.commit)
    .bind(&branch)
    .bind(trigger)
    .bind(Value::Object(stages).to_string())
    .bind(&subdomain)
    .bind(&node)
    .bind(deploy_after)
    .bind(keep)
    .bind(actor)
    .bind(ts)
    .bind(ts)
    .bind(pr_number)
    .execute(&mut *tx)
    .await?;
    if t.pending_changes {
        insert_issue(
            &mut tx,
            &id,
            &Issue {
                severity: "high".into(),
                stage: "analyze".into(),
                rule: "analyze/changed".into(),
                title: "設定有變動，請重新確認".into(),
                detail: "最新解析的 compose 與確認版本不同；這次驗證使用確認時的 commit。".into(),
                file: None,
                line: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    envs::refresh_assigned(s, &node).await?;

    let cmd = |c: &caloud_analyzer::VerifyCmd| VerifyCmd {
        image: c.image.clone(),
        setup: c.setup.clone(),
        command: c.command.clone(),
    };
    s.agents.send(
        &node,
        ServerMsg::Verify {
            run: Box::new(VerifySpec {
                id: id.clone(),
                env: EnvSpec {
                    id: subdomain,
                    kind: "verify".into(),
                    compose: t.compose,
                    subnet_index: index as u16,
                    public_service: t.public_service,
                    source: Some(EnvSource {
                        repo_url: p.repo_url.clone(),
                        commit: t.commit.clone(),
                        git_ref,
                        compose_dir: t.compose_dir,
                        auth_header,
                    }),
                    build: Some(build),
                    release: t
                        .verify
                        .release
                        .iter()
                        .map(|r| ReleaseTask {
                            service: r.service.clone(),
                            command: r.command.clone(),
                        })
                        .collect(),
                    vars,
                },
                public_port: t.public_port as u16,
                test: t.verify.test.as_ref().map(cmd),
                lint: t.verify.lint.as_ref().map(cmd),
                smoke: if t.verify.smoke.is_empty() {
                    vec!["/".into()]
                } else {
                    t.verify.smoke.clone()
                },
                keep_on_failure: keep,
            }),
        },
    );
    let detail = format!("{via}; {trigger} {} {}", branch, t.commit.chars().take(12).collect::<String>());
    audit(&s.db, actor, "project_verify", Some(&p.id), Some(detail.trim_start_matches("; "))).await;
    let v = get(s, &id).await?;
    // ADR 0021：GitHub check run（背景，失敗只記 log）
    if p.installation_id.is_some() {
        let (s2, p2, v2) = (s.clone(), p.clone(), v.clone());
        tokio::spawn(async move {
            if let Some(cid) = crate::github::checks::create(&s2, &p2, &v2).await {
                let _ = sqlx::query("UPDATE verifications SET check_run_id = ? WHERE id = ?")
                    .bind(cid)
                    .bind(&v2.id)
                    .execute(&s2.db)
                    .await;
            }
        });
    }
    Ok(v)
}

async fn insert_issue(tx: &mut sqlx::SqliteConnection, run: &str, i: &Issue) -> ApiResult<()> {
    let sev = if SEVERITIES.contains(&i.severity.as_str()) {
        i.severity.as_str()
    } else {
        "low"
    };
    sqlx::query(
        "INSERT OR IGNORE INTO verification_issues (verification, fingerprint, severity, stage, rule, title, detail, file, line) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(run)
    .bind(fingerprint(i))
    .bind(sev)
    .bind(i.stage.chars().take(20).collect::<String>())
    .bind(i.rule.chars().take(120).collect::<String>())
    .bind(i.title.chars().take(200).collect::<String>())
    .bind(i.detail.chars().take(1000).collect::<String>())
    .bind(i.file.as_ref().map(|f| f.chars().take(300).collect::<String>()))
    .bind(i.line.map(i64::from))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// agent 的回報只接受該節點上、仍在進行中的執行
async fn active(s: &AppState, node: &str, id: &str) -> Option<Verification> {
    let v = get(s, id).await.ok()?;
    (v.node.as_deref() == Some(node) && v.status == "running").then_some(v)
}

pub(crate) async fn on_stage(s: &AppState, node: &str, id: &str, stage: &str, status: &str) -> ApiResult<()> {
    if !VERIFY_STAGES.contains(&stage) || !["running", "passed", "failed", "skipped"].contains(&status) {
        return Ok(());
    }
    if active(s, node, id).await.is_none() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE verifications SET stages = json_set(stages, '$.' || ?1, ?2), stage = CASE WHEN ?2 = 'running' THEN ?1 ELSE stage END WHERE id = ?3",
    )
    .bind(stage)
    .bind(status)
    .bind(id)
    .execute(&s.db)
    .await?;
    Ok(())
}

pub(crate) async fn on_issue(s: &AppState, node: &str, id: &str, issue: Issue) -> ApiResult<()> {
    if active(s, node, id).await.is_none() {
        return Ok(());
    }
    let mut conn = s.db.acquire().await?;
    insert_issue(&mut conn, id, &issue).await
}

pub(crate) async fn on_log(s: &AppState, node: &str, id: &str, stage: &str, line: &str) -> ApiResult<()> {
    if !VERIFY_STAGES.contains(&stage) || active(s, node, id).await.is_none() {
        return Ok(());
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM verification_logs WHERE verification = ?")
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    if n >= MAX_LOGS {
        return Ok(());
    }
    sqlx::query("INSERT INTO verification_logs (verification, stage, line, at) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(stage)
        .bind(line.chars().take(500).collect::<String>())
        .bind(now())
        .execute(&s.db)
        .await?;
    Ok(())
}

async fn counts(s: &AppState, id: &str) -> ApiResult<Value> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT severity, COUNT(*) FROM verification_issues WHERE verification = ? GROUP BY severity")
            .bind(id)
            .fetch_all(&s.db)
            .await?;
    let mut m = Map::new();
    for sev in SEVERITIES {
        m.insert(sev.into(), rows.iter().find(|(k, _)| k == sev).map(|(_, n)| *n).unwrap_or(0).into());
    }
    Ok(Value::Object(m))
}

/// 刪除驗證環境紀錄；`kept` 時保留到期限
async fn release_env(s: &AppState, v: &Verification, kept: bool) -> ApiResult<()> {
    let Some(sub) = &v.env else { return Ok(()) };
    if kept {
        sqlx::query("UPDATE verifications SET keep_until = ? WHERE id = ?")
            .bind(now() + KEEP_SECS)
            .bind(&v.id)
            .execute(&s.db)
            .await?;
        return Ok(());
    }
    if let Ok(e) = envs::get(s, sub).await {
        envs::remove(s, &e).await?;
    }
    Ok(())
}

pub(crate) async fn on_done(s: &AppState, node: &str, id: &str, error: Option<String>, kept: bool) -> ApiResult<()> {
    let Ok(v) = get(s, id).await else { return Ok(()) };
    if v.node.as_deref() != Some(node) {
        return Ok(());
    }
    if finished(&v.status) {
        // 已取消或逾時：只釋放環境
        return release_env(s, &v, false).await;
    }
    let c = counts(s, id).await?;
    let status = match error.as_deref() {
        Some("已取消") => "canceled",
        Some(_) => "error",
        None => verdict(&c),
    };
    sqlx::query(
        "UPDATE verifications SET status = ?, error = ?, counts = ?, stage = NULL, finished_at = ?, \
         stages = json_set(stages, '$.report', 'passed') WHERE id = ?",
    )
    .bind(status)
    .bind(&error)
    .bind(c.to_string())
    .bind(now())
    .bind(id)
    .execute(&s.db)
    .await?;
    release_env(s, &v, kept).await?;
    notify_github(s, id).await;
    audit(
        &s.db,
        None,
        "verification_done",
        Some(&v.project),
        Some(&format!(
            "via=agent; {status} {} blocker={} high={}",
            v.commit_sha.chars().take(12).collect::<String>(),
            c["blocker"],
            c["high"]
        )),
    )
    .await;
    if status == "passed" && v.deploy_after {
        if let Ok(p) = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
            .bind(&v.project)
            .fetch_one(&s.db)
            .await
        {
            // 確認的設定在驗證期間沒有變
            if p.confirmed_analysis.as_deref() == Some(v.analysis.as_str()) {
                let via = if v.trigger == "push" { "github" } else { "web" };
                deploy::deploy_verified(s, &p, via).await;
            }
        }
    }
    Ok(())
}

/// ADR 0021：完成 GitHub check run 與 PR 留言（背景）
async fn notify_github(s: &AppState, id: &str) {
    let Ok(v) = get(s, id).await else { return };
    let Ok(p) = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = ?")
        .bind(&v.project)
        .fetch_one(&s.db)
        .await
    else {
        return;
    };
    if p.installation_id.is_none() {
        return;
    }
    let list: Vec<Value> = issues(s, id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    let s2 = s.clone();
    tokio::spawn(async move { crate::github::checks::complete(&s2, &p, &v, &list).await });
}

/// `GET /api/projects/{id}/verifications`
pub async fn list(s: &AppState, me: &CurrentUser, project: &str) -> ApiResult<Vec<Verification>> {
    owned(s, me, project).await?;
    Ok(
        sqlx::query_as("SELECT * FROM verifications WHERE project = ? ORDER BY created_at DESC, rowid DESC LIMIT ?")
            .bind(project)
            .bind(KEEP_RUNS)
            .fetch_all(&s.db)
            .await?,
    )
}

/// 專案頁：最近一次與目標 commit 是否已通過
pub(crate) async fn summary(s: &AppState, p: &Project) -> ApiResult<Value> {
    let latest: Option<Verification> =
        sqlx::query_as("SELECT * FROM verifications WHERE project = ? ORDER BY created_at DESC, rowid DESC LIMIT 1")
            .bind(&p.id)
            .fetch_optional(&s.db)
            .await?;
    let verified = match deploy::target(s, p).await {
        Ok(t) => passed(s, &p.id, &t.commit, &t.analysis).await?,
        Err(_) => false,
    };
    Ok(json!({ "latest": latest, "verified": verified }))
}

async fn issues(s: &AppState, id: &str) -> ApiResult<Vec<IssueRow>> {
    Ok(sqlx::query_as(
        "SELECT fingerprint, severity, stage, rule, title, detail, file, line FROM verification_issues WHERE verification = ? \
         ORDER BY CASE severity WHEN 'blocker' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END, id",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?)
}

/// 同分支上一次完成的執行（FR-RPT-02）
async fn previous(s: &AppState, v: &Verification) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM verifications WHERE project = ? AND branch = ? AND status IN ('passed', 'failed') AND created_at <= ? AND id != ? \
         ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(&v.project)
    .bind(&v.branch)
    .bind(v.created_at)
    .bind(&v.id)
    .fetch_optional(&s.db)
    .await?)
}

/// `GET /api/verifications/{id}`
pub async fn detail(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Value> {
    let v = owned_run(s, me, id).await?;
    let cur = issues(s, id).await?;
    let prev = previous(s, &v).await?;
    let before: Vec<IssueRow> = match &prev {
        Some(p) => issues(s, p).await?,
        None => vec![],
    };
    let old: HashSet<&str> = before.iter().map(|i| i.fingerprint.as_str()).collect();
    let now_fp: HashSet<&str> = cur.iter().map(|i| i.fingerprint.as_str()).collect();
    let tagged: Vec<Value> = cur
        .iter()
        .map(|i| {
            let mut j = serde_json::to_value(i).unwrap_or_default();
            j["change"] = if prev.is_none() || !old.contains(i.fingerprint.as_str()) {
                "new"
            } else {
                "existing"
            }
            .into();
            j
        })
        .collect();
    let fixed: Vec<&IssueRow> = if finished(&v.status) {
        before.iter().filter(|i| !now_fp.contains(i.fingerprint.as_str())).collect()
    } else {
        vec![]
    };
    let mut logs: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT stage, line, at FROM verification_logs WHERE verification = ? ORDER BY id DESC LIMIT ?")
            .bind(id)
            .bind(SHOW_LOGS)
            .fetch_all(&s.db)
            .await?;
    logs.reverse();
    Ok(json!({
        "verification": v,
        "issues": tagged,
        "fixed": fixed,
        "previous": prev,
        "logs": logs.into_iter().map(|(stage, line, at)| json!({ "stage": stage, "line": line, "at": at })).collect::<Vec<_>>(),
    }))
}

/// `POST /api/verifications/{id}/cancel`
pub async fn cancel(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<()> {
    let v = owned_run(s, me, id).await?;
    if finished(&v.status) {
        return Err(conflict("這次驗證已經結束。"));
    }
    cancel_run(s, &v, "使用者取消").await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "verification_cancel",
        Some(&v.project),
        me.audit_detail(Some(id)).as_deref(),
    )
    .await;
    Ok(())
}

/// Markdown 報告（FR-RPT-03）
pub async fn report_md(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<String> {
    let d = detail(s, me, id).await?;
    let v = &d["verification"];
    let name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = ?")
        .bind(v["project"].as_str().unwrap_or_default())
        .fetch_one(&s.db)
        .await?;
    let mut md = format!(
        "# 驗證報告：{name}\n\n- 結果：**{}**\n- Commit：`{}`（{}）\n- 觸發：{}\n- 問題：Blocker {}、High {}、Medium {}、Low {}\n",
        v["status"].as_str().unwrap_or_default(),
        v["commit"].as_str().unwrap_or_default(),
        v["branch"].as_str().unwrap_or_default(),
        v["trigger"].as_str().unwrap_or_default(),
        v["counts"]["blocker"],
        v["counts"]["high"],
        v["counts"]["medium"],
        v["counts"]["low"],
    );
    if let Some(e) = v["error"].as_str() {
        md.push_str(&format!("- 錯誤：{e}\n"));
    }
    md.push_str("\n## 階段\n\n| 階段 | 狀態 |\n| --- | --- |\n");
    for st in VERIFY_STAGES {
        md.push_str(&format!("| {st} | {} |\n", v["stages"][st].as_str().unwrap_or("pending")));
    }
    md.push_str("\n## 問題\n\n");
    let list = d["issues"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        md.push_str("沒有發現問題。\n");
    } else {
        md.push_str("| 等級 | 變化 | 階段 | 問題 | 位置 |\n| --- | --- | --- | --- | --- |\n");
        let cell = |x: &str| x.replace('|', "\\|").replace('\n', " ");
        for i in &list {
            let loc = match (i["file"].as_str(), i["line"].as_i64()) {
                (Some(f), Some(l)) => format!("`{f}:{l}`"),
                (Some(f), None) => format!("`{f}`"),
                _ => String::new(),
            };
            md.push_str(&format!(
                "| {} | {} | {} | {}{} | {} |\n",
                i["severity"].as_str().unwrap_or_default(),
                if i["change"] == "new" { "新增" } else { "仍存在" },
                i["stage"].as_str().unwrap_or_default(),
                cell(i["title"].as_str().unwrap_or_default()),
                match i["detail"].as_str() {
                    Some(d) if !d.is_empty() => format!("<br>{}", cell(d)),
                    _ => String::new(),
                },
                loc
            ));
        }
    }
    let fixed = d["fixed"].as_array().cloned().unwrap_or_default();
    if !fixed.is_empty() {
        md.push_str("\n## 已修正（相對前次）\n\n");
        for i in fixed {
            md.push_str(&format!(
                "- {}：{}\n",
                i["severity"].as_str().unwrap_or_default(),
                i["title"].as_str().unwrap_or_default()
            ));
        }
    }
    Ok(md)
}

/// 背景巡檢（每分鐘）
pub fn spawn_sweeper(s: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            if let Err(e) = sweep(&s).await {
                tracing::warn!(error = %e, "verification sweep failed");
            }
        }
    });
}

/// 逾時或節點離線 → error；到期的保留環境 → destroy；每個專案保留最近 50 次
pub async fn sweep(s: &AppState) -> ApiResult<()> {
    let t = now();
    let running: Vec<Verification> = sqlx::query_as("SELECT * FROM verifications WHERE status IN ('queued', 'running')")
        .fetch_all(&s.db)
        .await?;
    for v in running {
        let offline = v.node.as_deref().is_some_and(|n| !s.agents.is_connected(n));
        let late = t - v.started_at.unwrap_or(v.created_at) > RUN_TIMEOUT;
        if offline || late {
            let why = if offline { "Runner 節點離線" } else { "驗證逾時" };
            sqlx::query("UPDATE verifications SET status = 'error', error = ?, finished_at = ?, stage = NULL WHERE id = ?")
                .bind(why)
                .bind(t)
                .bind(&v.id)
                .execute(&s.db)
                .await?;
            if let Some(node) = v.node.as_deref().filter(|n| s.agents.is_connected(n)) {
                s.agents.send(node, ServerMsg::CancelVerify { id: v.id.clone() });
            } else {
                release_env(s, &v, false).await?;
            }
        }
    }
    let expired: Vec<Verification> = sqlx::query_as("SELECT * FROM verifications WHERE keep_until IS NOT NULL AND keep_until < ?")
        .bind(t)
        .fetch_all(&s.db)
        .await?;
    for v in expired {
        sqlx::query("UPDATE verifications SET keep_until = NULL WHERE id = ?")
            .bind(&v.id)
            .execute(&s.db)
            .await?;
        if let Some(sub) = &v.env {
            if let Ok(e) = envs::get(s, sub).await {
                envs::destroy(s, &e).await?;
            }
        }
    }
    sqlx::query(
        "DELETE FROM verifications WHERE id IN (SELECT id FROM (SELECT id, ROW_NUMBER() OVER (PARTITION BY project ORDER BY created_at DESC, rowid DESC) AS n \
         FROM verifications) WHERE n > ?) AND status NOT IN ('queued', 'running') AND keep_until IS NULL",
    )
    .bind(KEEP_RUNS)
    .execute(&s.db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-RPT-01
    #[test]
    fn fingerprints_and_verdict() {
        let i = |title: &str, file: Option<&str>| Issue {
            severity: "high".into(),
            stage: "logs".into(),
            rule: "logs/restart".into(),
            title: title.into(),
            detail: "x".into(),
            file: file.map(str::to_string),
            line: Some(3),
        };
        assert_eq!(
            fingerprint(&i("`web-1` 重啟了 2 次", None)),
            fingerprint(&i("`web-1` 重啟了 5 次", None))
        );
        assert_ne!(fingerprint(&i("a", Some("x.ts"))), fingerprint(&i("a", Some("y.ts"))));
        assert_eq!(fingerprint(&i("a", None)).len(), 16);
        assert_eq!(verdict(&json!({"blocker": 0, "high": 0, "medium": 3})), "passed");
        assert_eq!(verdict(&json!({"blocker": 1})), "failed");
        assert_eq!(verdict(&json!({"high": 2})), "failed");
    }
}

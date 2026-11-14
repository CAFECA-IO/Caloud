//! 平臺專案：以公開儲存庫匯入、背景解析、確認（FR-GH-04、FR-AN-03、FR-AN-04、FR-AN-06、FR-AN-10）。
//!
//! 規格見 docs/06-api-spec.md §3.7。REST handler 只經由這裡操作專案。
use super::{check_git_ref, check_repo_path, check_repo_url};
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_analyzer::{Analysis, Confidence};
use caloud_core::db::audit;
use caloud_core::model::{AnalysisRow, Project};
use caloud_core::now;
use caloud_orchestrator::{validate_with, Policy};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

/// 每位用戶的專案上限（FR-AN-10）
pub const MAX_PROJECTS: i64 = 20;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::BadRequest(msg.into())
}

/// 專案原始碼目錄
fn src_dir(s: &AppState, id: &str) -> PathBuf {
    s.cfg.data_dir.join("projects").join(id)
}

/// 從網址取儲存庫名稱：`https://github.com/a/b.git` → `b`
fn repo_name(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .chars()
        .take(64)
        .collect()
}

fn check_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 {
        return Err(bad("專案名稱為 1–64 個字。"));
    }
    Ok(n.to_string())
}

/// 取得專案並確認為擁有者或 admin+
pub async fn owned(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Project> {
    me.require_deploy()?;
    let p: Project = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    me.can_manage(&p.owner).then_some(p).ok_or(ApiError::Forbidden)
}

/// admin+ 看全部，其餘只看自己的
pub async fn list(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<Project>> {
    me.require_deploy()?;
    let rows = if me.role().is_admin() {
        sqlx::query_as("SELECT * FROM projects ORDER BY created_at DESC")
            .fetch_all(&s.db)
            .await?
    } else {
        sqlx::query_as("SELECT * FROM projects WHERE owner = ? ORDER BY created_at DESC")
            .bind(&me.0.account)
            .fetch_all(&s.db)
            .await?
    };
    Ok(rows)
}

#[derive(Debug, Default, Deserialize)]
pub struct ProjectInput {
    pub repo_url: Option<String>,
    pub git_ref: Option<String>,
    pub root: Option<String>,
    pub name: Option<String>,
}

/// 空字串視為「未指定」
fn opt(v: &Option<String>) -> Option<String> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// FR-GH-04：建立專案並在背景解析
pub async fn create(s: &AppState, me: &CurrentUser, input: ProjectInput) -> ApiResult<Project> {
    me.require_deploy()?;
    let url = opt(&input.repo_url).ok_or_else(|| bad("請提供儲存庫網址。"))?;
    check_repo_url(&url)?;
    let git_ref = opt(&input.git_ref);
    if let Some(r) = &git_ref {
        check_git_ref(r)?;
    }
    let root = opt(&input.root).unwrap_or_default().trim_matches('/').to_string();
    check_repo_path(&root)?;
    let name = check_name(&opt(&input.name).unwrap_or_else(|| repo_name(&url)))?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE owner = ?")
        .bind(&me.0.account)
        .fetch_one(&s.db)
        .await?;
    if count >= MAX_PROJECTS {
        return Err(ApiError::Conflict(format!(
            "每位用戶最多 {MAX_PROJECTS} 個專案，請先刪除不用的專案。"
        )));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let t = now();
    sqlx::query(
        "INSERT INTO projects (id, owner, name, repo_url, git_ref, root, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, 'analyzing', ?, ?)",
    )
    .bind(&id)
    .bind(&me.0.account)
    .bind(&name)
    .bind(&url)
    .bind(&git_ref)
    .bind(&root)
    .bind(t)
    .bind(t)
    .execute(&s.db)
    .await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "project_create",
        Some(&id),
        me.audit_detail(Some(&url)).as_deref(),
    )
    .await;
    let p = get_project(s, &id).await?;
    spawn_analysis(s.clone(), p.clone());
    Ok(p)
}

async fn get_project(s: &AppState, id: &str) -> ApiResult<Project> {
    Ok(sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(id)
        .fetch_one(&s.db)
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

fn analysis_json(a: &AnalysisRow) -> Value {
    let result: Value = serde_json::from_str(&a.result).unwrap_or(Value::Null);
    let acknowledged: Value = serde_json::from_str(&a.acknowledged).unwrap_or_else(|_| json!([]));
    json!({
        "id": a.id, "git_ref": a.git_ref, "commit": a.commit_sha, "created_at": a.created_at,
        "result": result, "compose": a.compose, "public_service": a.public_service,
        "acknowledged": acknowledged, "confirmed_by": a.confirmed_by, "confirmed_at": a.confirmed_at,
    })
}

/// 專案、最新一筆解析、已確認的解析
pub async fn get(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Value> {
    let p = owned(s, me, id).await?;
    let last = latest(s, id).await?;
    let confirmed: Option<AnalysisRow> = match &p.confirmed_analysis {
        Some(a) => {
            sqlx::query_as("SELECT * FROM analyses WHERE id = ?")
                .bind(a)
                .fetch_optional(&s.db)
                .await?
        }
        None => None,
    };
    Ok(json!({
        "project": p,
        "analysis": last.as_ref().map(analysis_json),
        "confirmed": confirmed.as_ref().map(analysis_json),
    }))
}

/// FR-AN-06：改名稱、ref 或根目錄；ref 或根目錄改變時重新解析
pub async fn update(s: &AppState, me: &CurrentUser, id: &str, input: ProjectInput) -> ApiResult<Project> {
    let p = owned(s, me, id).await?;
    let name = match &input.name {
        Some(n) => check_name(n)?,
        None => p.name.clone(),
    };
    let git_ref = match &input.git_ref {
        Some(_) => opt(&input.git_ref),
        None => p.git_ref.clone(),
    };
    if let Some(r) = &git_ref {
        check_git_ref(r)?;
    }
    let root = match &input.root {
        Some(r) => r.trim().trim_matches('/').to_string(),
        None => p.root.clone(),
    };
    check_repo_path(&root)?;
    let reanalyze = git_ref != p.git_ref || root != p.root;
    if reanalyze && p.status == "analyzing" {
        return Err(ApiError::Conflict("正在解析，請等這次完成後再修改。".into()));
    }
    sqlx::query("UPDATE projects SET name = ?, git_ref = ?, root = ?, updated_at = ? WHERE id = ?")
        .bind(&name)
        .bind(&git_ref)
        .bind(&root)
        .bind(now())
        .bind(id)
        .execute(&s.db)
        .await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "project_update",
        Some(id),
        me.audit_detail(None).as_deref(),
    )
    .await;
    if reanalyze {
        start(s, id).await?;
    }
    get_project(s, id).await
}

/// 標為解析中並在背景解析
async fn start(s: &AppState, id: &str) -> ApiResult<()> {
    let n = sqlx::query("UPDATE projects SET status = 'analyzing', error = NULL, updated_at = ? WHERE id = ? AND status != 'analyzing'")
        .bind(now())
        .bind(id)
        .execute(&s.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::Conflict("正在解析，請稍候。".into()));
    }
    spawn_analysis(s.clone(), get_project(s, id).await?);
    Ok(())
}

/// FR-AN-10：以目前設定重新解析
pub async fn reanalyze(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Project> {
    owned(s, me, id).await?;
    start(s, id).await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "project_analyze",
        Some(id),
        me.audit_detail(None).as_deref(),
    )
    .await;
    get_project(s, id).await
}

pub async fn delete(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<()> {
    let p = owned(s, me, id).await?;
    sqlx::query("DELETE FROM analyses WHERE project = ?")
        .bind(id)
        .execute(&s.db)
        .await?;
    sqlx::query("DELETE FROM projects WHERE id = ?").bind(id).execute(&s.db).await?;
    let _ = tokio::fs::remove_dir_all(src_dir(s, id)).await;
    audit(
        &s.db,
        Some(&me.0.account),
        "project_delete",
        Some(id),
        me.audit_detail(Some(&p.name)).as_deref(),
    )
    .await;
    Ok(())
}

/// 背景：取得儲存庫 → 解析 → 寫入一筆 analyses
fn spawn_analysis(s: AppState, p: Project) {
    tokio::spawn(async move {
        let out = run_analysis(&s, &p).await;
        let t = now();
        let res = match out {
            Ok(()) => {
                sqlx::query(
                    "UPDATE projects SET status = CASE WHEN confirmed_analysis IS NULL THEN 'needs_review' ELSE 'confirmed' END, \
                     error = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(t)
                .bind(&p.id)
                .execute(&s.db)
                .await
            }
            Err(msg) => {
                tracing::warn!(project = %p.id, error = %msg, "analysis failed");
                sqlx::query("UPDATE projects SET status = 'error', error = ?, updated_at = ? WHERE id = ?")
                    .bind(&msg)
                    .bind(t)
                    .bind(&p.id)
                    .execute(&s.db)
                    .await
            }
        };
        if let Err(e) = res {
            tracing::error!(project = %p.id, error = %e, "failed to record analysis");
        }
    });
}

/// 失敗時回傳給用戶看的訊息（NFR-SEC-03：不含主機路徑等內部細節）
async fn run_analysis(s: &AppState, p: &Project) -> Result<(), String> {
    let dir = src_dir(s, &p.id).join("src");
    let fetched = caloud_orchestrator::git::fetch_repo(&s.runner.git_bin, &dir, &p.repo_url, p.git_ref.as_deref())
        .await
        .map_err(|e| {
            let msg = e.to_string();
            let msg = msg.replace(&dir.display().to_string(), "…");
            format!("無法取得儲存庫：{}", msg.lines().next().unwrap_or_default())
        })?;
    let root = p.root.clone();
    let d = dir.clone();
    let analysis = tokio::task::spawn_blocking(move || caloud_analyzer::analyze(&d, &caloud_analyzer::Options { root }))
        .await
        .map_err(|_| "解析時發生錯誤，請稍後再試。".to_string())?;
    let result = serde_json::to_string(&analysis).map_err(|_| "解析結果無法保存。".to_string())?;
    let git_ref = p.git_ref.clone().unwrap_or_else(|| "HEAD".into());
    let t = now();
    let ins = sqlx::query("INSERT INTO analyses (id, project, git_ref, commit_sha, result, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&p.id)
        .bind(&git_ref)
        .bind(&fetched.commit)
        .bind(&result)
        .bind(t)
        .execute(&s.db)
        .await;
    let branch = sqlx::query("UPDATE projects SET branch = COALESCE(?, branch) WHERE id = ?")
        .bind(&fetched.branch)
        .bind(&p.id)
        .execute(&s.db)
        .await;
    ins.and(branch).map(|_| ()).map_err(|e| {
        tracing::error!(error = %e, "analysis insert failed");
        "解析結果無法保存。".to_string()
    })
}

#[derive(Debug, Deserialize)]
pub struct ConfirmInput {
    pub compose: Option<String>,
    pub public_service: Option<String>,
    #[serde(default)]
    pub acknowledged: Vec<usize>,
}

/// FR-AN-03、FR-AN-04：確認最新一筆解析（可附用戶編輯的 compose）
pub async fn confirm(s: &AppState, me: &CurrentUser, id: &str, input: ConfirmInput) -> ApiResult<Project> {
    let p = owned(s, me, id).await?;
    if p.status == "analyzing" {
        return Err(ApiError::Conflict("正在解析，請等這次完成後再確認。".into()));
    }
    let row = latest(s, id).await?.ok_or_else(|| ApiError::Conflict("還沒有解析結果。".into()))?;
    let analysis: Analysis = serde_json::from_str(&row.result).map_err(|e| ApiError::Internal(e.into()))?;

    // 低信心項目全部確認
    let missing: Vec<String> = analysis
        .findings
        .iter()
        .enumerate()
        .filter(|(i, f)| f.confidence == Confidence::Low && !input.acknowledged.contains(i))
        .map(|(i, f)| format!("#{i} {}", f.message))
        .collect();
    if !missing.is_empty() {
        return Err(bad(format!("以下低信心項目尚未確認：{}", missing.join("；"))));
    }

    // compose：用戶編輯版或產生的版本，都以平臺政策檢查
    let edited = input.compose.clone().filter(|c| !c.trim().is_empty());
    if let Some(c) = &edited {
        if c.len() > 256 * 1024 {
            return Err(bad("compose 過長（上限 256 KB）。"));
        }
    }
    let yaml = edited
        .clone()
        .or_else(|| analysis.compose.clone())
        .ok_or_else(|| bad("解析有阻擋項，請先編輯 compose 解決後再確認。"))?;
    let depth = analysis.compose_dir.split('/').filter(|c| !c.is_empty()).count();
    let compose = validate_with(&yaml, Policy::platform(depth)).map_err(|e| bad(e.to_string()))?;
    let public = opt(&input.public_service).ok_or_else(|| bad("請選擇對外服務。"))?;
    if !compose.services.contains(&public) {
        return Err(bad(format!("compose 中沒有服務 `{public}`。")));
    }

    let edited = edited.filter(|c| Some(c) != analysis.compose.as_ref());
    let t = now();
    let ack = serde_json::to_string(&input.acknowledged).unwrap_or_else(|_| "[]".into());
    sqlx::query("UPDATE analyses SET compose = ?, public_service = ?, acknowledged = ?, confirmed_by = ?, confirmed_at = ? WHERE id = ?")
        .bind(&edited)
        .bind(&public)
        .bind(&ack)
        .bind(&me.0.account)
        .bind(t)
        .bind(&row.id)
        .execute(&s.db)
        .await?;
    sqlx::query("UPDATE projects SET status = 'confirmed', confirmed_analysis = ?, updated_at = ? WHERE id = ?")
        .bind(&row.id)
        .bind(t)
        .bind(id)
        .execute(&s.db)
        .await?;
    let detail = format!(
        "{}{}",
        row.commit_sha.chars().take(12).collect::<String>(),
        if edited.is_some() { " compose 已編輯" } else { "" }
    );
    audit(
        &s.db,
        Some(&me.0.account),
        "project_confirm",
        Some(id),
        me.audit_detail(Some(&detail)).as_deref(),
    )
    .await;
    get_project(s, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_urls() {
        assert_eq!(repo_name("https://github.com/CAFECA-IO/SwarmStorage"), "SwarmStorage");
        assert_eq!(repo_name("https://github.com/a/b.git/"), "b");
    }
}

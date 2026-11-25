//! GitHub Deployments（FR-GH-06，[ADR 0023](../../../../docs/adr/0023-versions.md)）。
//!
//! 只限以 GitHub 安裝匯入的專案；API 失敗只記錄 log，不影響部署。
//! 建立 deployment 與之後的狀態都在背景執行；結束時 deployment 尚未建立就最多等 30 秒。
use super::app::gh_post;
use super::checks::auth;
use crate::AppState;
use caloud_core::model::Project;
use serde_json::json;
use std::time::Duration;

/// GitHub 的 environment 名稱
pub const ENVIRONMENT: &str = "production";

/// 專案頁網址（log_url）
fn project_url(s: &AppState, project: &str) -> String {
    format!("{}/#projects/{project}", s.cfg.public_origin.trim_end_matches('/'))
}

/// `description` 上限 140 字
fn short(s: &str) -> String {
    s.chars().take(140).collect()
}

/// 部署開始：建立 deployment，接著送出 `in_progress`
pub(crate) fn spawn_create(s: AppState, p: Project, dep_id: String, commit: String, env_url: String) {
    if p.installation_id.is_none() {
        return;
    }
    tokio::spawn(async move {
        let Some((token, repo)) = auth(&s, &p).await else { return };
        let body = json!({
            "ref": commit,
            "environment": ENVIRONMENT,
            "auto_merge": false,
            "required_contexts": [],
            "production_environment": true,
            "transient_environment": false,
            "description": "Caloud",
            "payload": { "caloud_deployment": dep_id },
        });
        let gh_id = match gh_post(&s, &format!("/repos/{repo}/deployments"), &token, body).await {
            Ok(v) => match v["id"].as_i64() {
                Some(id) => id,
                None => return tracing::warn!(project = %p.id, "github deployment: no id"),
            },
            Err(e) => return tracing::warn!(project = %p.id, error = %e, "github deployment create failed"),
        };
        if let Err(e) = sqlx::query("UPDATE deployments SET gh_deployment_id = ? WHERE id = ?")
            .bind(gh_id)
            .bind(&dep_id)
            .execute(&s.db)
            .await
        {
            tracing::warn!(error = %e, "github deployment id not saved");
        }
        let status = json!({
            "state": "in_progress",
            "log_url": project_url(&s, &p.id),
            "environment_url": env_url,
            "description": "Caloud 部署中",
        });
        if let Err(e) = gh_post(&s, &format!("/repos/{repo}/deployments/{gh_id}/statuses"), &token, status).await {
            tracing::warn!(project = %p.id, error = %e, "github deployment status failed");
        }
    });
}

/// 部署結束：`success` 或 `failure`
pub(crate) fn spawn_finish(s: AppState, p: Project, dep_id: String, ok: bool, description: String, env_url: String) {
    if p.installation_id.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut gh_id = None;
        for _ in 0..30 {
            gh_id = sqlx::query_scalar::<_, Option<i64>>("SELECT gh_deployment_id FROM deployments WHERE id = ?")
                .bind(&dep_id)
                .fetch_optional(&s.db)
                .await
                .ok()
                .flatten()
                .flatten();
            if gh_id.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let Some(gh_id) = gh_id else {
            return tracing::warn!(deployment = %dep_id, "github deployment never created; status skipped");
        };
        let Some((token, repo)) = auth(&s, &p).await else { return };
        let status = json!({
            "state": if ok { "success" } else { "failure" },
            "log_url": project_url(&s, &p.id),
            "environment_url": env_url,
            "description": short(&description),
            "auto_inactive": true,
        });
        if let Err(e) = gh_post(&s, &format!("/repos/{repo}/deployments/{gh_id}/statuses"), &token, status).await {
            tracing::warn!(project = %p.id, error = %e, "github deployment status failed");
        }
    });
}

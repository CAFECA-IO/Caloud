//! GitHub webhook（FR-GH-02，ADR 0017）：驗簽、去重、事件分派。
use super::app::creds;
use crate::AppState;
use axum::http::{HeaderMap, StatusCode};
use caloud_core::db::audit;
use caloud_core::now;
use serde_json::Value;

/// 本體上限
pub const MAX_BODY: usize = 5 * 1024 * 1024;
const KEEP_DELIVERIES: i64 = 7 * 86400;

/// `sha256=<hex>` 以常數時間比較
pub fn verify(secret: &str, body: &[u8], sig_header: Option<&str>) -> bool {
    let Some(hex_sig) = sig_header.and_then(|h| h.strip_prefix("sha256=")) else {
        return false;
    };
    let Ok(tag) = hex::decode(hex_sig) else { return false };
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    ring::hmac::verify(&key, body, &tag).is_ok()
}

fn header<'a>(h: &'a HeaderMap, k: &str) -> Option<&'a str> {
    h.get(k).and_then(|v| v.to_str().ok())
}

/// 回傳狀態碼與給 GitHub 看的簡短訊息
pub async fn handle(s: &AppState, headers: &HeaderMap, body: &[u8]) -> (StatusCode, &'static str) {
    let c = match creds(s).await {
        Ok(Some(c)) => c,
        _ => return (StatusCode::SERVICE_UNAVAILABLE, "github app not configured"),
    };
    if !verify(&c.webhook_secret, body, header(headers, "x-hub-signature-256")) {
        return (StatusCode::UNAUTHORIZED, "bad signature");
    }
    let event = header(headers, "x-github-event").unwrap_or_default().to_string();
    let Some(delivery) = header(headers, "x-github-delivery").filter(|d| !d.is_empty() && d.len() <= 64) else {
        return (StatusCode::BAD_REQUEST, "missing delivery");
    };
    let t = now();
    let _ = sqlx::query("DELETE FROM github_deliveries WHERE received_at < ?")
        .bind(t - KEEP_DELIVERIES)
        .execute(&s.db)
        .await;
    match sqlx::query("INSERT OR IGNORE INTO github_deliveries (delivery_id, event, received_at) VALUES (?, ?, ?)")
        .bind(delivery)
        .bind(&event)
        .bind(t)
        .execute(&s.db)
        .await
    {
        Ok(r) if r.rows_affected() == 0 => return (StatusCode::OK, "duplicate"),
        Ok(_) => {}
        Err(e) => {
            tracing::error!(error = %e, "failed to record webhook delivery");
            return (StatusCode::INTERNAL_SERVER_ERROR, "error");
        }
    }
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return (StatusCode::BAD_REQUEST, "bad json");
    };
    if let Err(e) = dispatch(s, &event, &v).await {
        tracing::warn!(error = %e, %event, "webhook handling failed");
    }
    (StatusCode::OK, "ok")
}

async fn dispatch(s: &AppState, event: &str, v: &Value) -> anyhow::Result<()> {
    let installation = v["installation"]["id"].as_i64();
    let action = v["action"].as_str().unwrap_or_default();
    match event {
        "installation" => {
            let Some(id) = installation else { return Ok(()) };
            match action {
                "deleted" => {
                    sqlx::query("DELETE FROM github_installations WHERE id = ?")
                        .bind(id)
                        .execute(&s.db)
                        .await?;
                    s.github.forget_token(id);
                }
                "suspend" | "unsuspend" => {
                    sqlx::query("UPDATE github_installations SET suspended = ? WHERE id = ?")
                        .bind(action == "suspend")
                        .bind(id)
                        .execute(&s.db)
                        .await?;
                    s.github.forget_token(id);
                }
                _ => {}
            }
            audit(&s.db, None, "github_installation", Some(&id.to_string()), Some(action)).await;
        }
        "push" | "create" | "pull_request" => {
            let Some(id) = installation else { return Ok(()) };
            let repo = v["repository"]["full_name"].as_str().unwrap_or_default();
            let default_branch = v["repository"]["default_branch"].as_str().unwrap_or_default();
            let branch = match event {
                "push" => v["ref"].as_str().and_then(|r| r.strip_prefix("refs/heads/")).map(str::to_string),
                "create" => (v["ref_type"] == "branch").then(|| v["ref"].as_str().unwrap_or_default().to_string()),
                _ => ["opened", "synchronize", "reopened"]
                    .contains(&action)
                    .then(|| v["pull_request"]["head"]["ref"].as_str().unwrap_or_default().to_string()),
            };
            let Some(branch) = branch.filter(|b| !b.is_empty()) else {
                return Ok(());
            };
            let projects: Vec<caloud_core::model::Project> = sqlx::query_as(
                "SELECT * FROM projects WHERE installation_id = ? AND repo_full_name = ? AND status != 'analyzing'
                   AND (git_ref = ? OR (git_ref IS NULL AND ? = ?))",
            )
            .bind(id)
            .bind(repo)
            .bind(&branch)
            .bind(&branch)
            .bind(default_branch)
            .fetch_all(&s.db)
            .await?;
            for p in projects {
                sqlx::query("UPDATE projects SET status = 'analyzing', error = NULL, updated_at = ? WHERE id = ?")
                    .bind(now())
                    .bind(&p.id)
                    .execute(&s.db)
                    .await?;
                audit(
                    &s.db,
                    None,
                    "github_event",
                    Some(&p.id),
                    Some(&format!("via=github; {event} {branch}")),
                )
                .await;
                crate::service::projects::spawn_analysis(s.clone(), p);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verify;

    // FR-GH-02
    #[test]
    fn verifies_hmac_signature() {
        // GitHub 文件的範例：secret "It's a Secret to Everybody"、body "Hello, World!"
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(verify("It's a Secret to Everybody", b"Hello, World!", Some(sig)));
        assert!(!verify("It's a Secret to Everybody", b"Hello, World?", Some(sig)));
        assert!(!verify("other", b"Hello, World!", Some(sig)));
        assert!(!verify("It's a Secret to Everybody", b"Hello, World!", None));
        assert!(!verify("It's a Secret to Everybody", b"Hello, World!", Some("sha1=abc")));
        assert!(!verify("It's a Secret to Everybody", b"Hello, World!", Some("sha256=zz")));
    }
}

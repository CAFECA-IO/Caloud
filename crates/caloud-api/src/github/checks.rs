//! GitHub Checks 與 PR 留言（FR-GH-03，[ADR 0021](../../../../docs/adr/0021-checks-quota-release.md)）。
//!
//! 只限以 GitHub 安裝匯入的專案；API 失敗只記錄 log，不影響驗證。
use super::app::{gh_patch, gh_post, installation_token};
use crate::service::verify::Verification;
use crate::AppState;
use caloud_core::model::Project;
use serde_json::{json, Value};

pub const CHECK_NAME: &str = "Caloud 驗證";
/// 留言中用來辨識自己的標記
const MARKER: &str = "<!-- caloud-verification -->";

async fn auth(s: &AppState, p: &Project) -> Option<(String, String)> {
    let inst = p.installation_id?;
    let repo = p.repo_full_name.clone()?;
    let token = installation_token(s, inst)
        .await
        .map_err(|e| tracing::warn!(project = %p.id, error = %e, "github checks: no token"))
        .ok()?;
    Some((format!("Bearer {token}"), repo))
}

/// 報告頁網址
pub fn report_url(s: &AppState, v: &Verification) -> String {
    format!(
        "{}/#projects/{}/verifications/{}",
        s.cfg.public_origin.trim_end_matches('/'),
        v.project,
        v.id
    )
}

/// 驗證結果 → check run conclusion
pub fn conclusion(status: &str) -> &'static str {
    match status {
        "passed" => "success",
        "failed" => "failure",
        "canceled" => "cancelled",
        _ => "neutral",
    }
}

/// 摘要（GitHub 上限 65535 字）
pub fn summary(s: &AppState, v: &Verification, issues: &[Value]) -> (String, String) {
    let c: Value = serde_json::from_str(&v.counts).unwrap_or_default();
    let n = |k: &str| c[k].as_i64().unwrap_or(0);
    let title = match v.status.as_str() {
        "passed" => format!("通過（Medium {}、Low {}）", n("medium"), n("low")),
        "failed" => format!("未通過：Blocker {}、High {}", n("blocker"), n("high")),
        "canceled" => "已取消".into(),
        _ => format!("錯誤：{}", v.error.as_deref().unwrap_or("驗證未完成")),
    };
    let mut body = format!(
        "**{title}**\n\nBlocker {}・High {}・Medium {}・Low {}\n\n",
        n("blocker"),
        n("high"),
        n("medium"),
        n("low")
    );
    let cell = |x: &str| x.replace('|', "\\|").replace('\n', " ");
    if !issues.is_empty() {
        body.push_str("| 等級 | 階段 | 問題 | 位置 |\n| --- | --- | --- | --- |\n");
        for i in issues.iter().take(20) {
            let loc = match (i["file"].as_str(), i["line"].as_i64()) {
                (Some(f), Some(l)) => format!("`{f}:{l}`"),
                (Some(f), None) => format!("`{f}`"),
                _ => String::new(),
            };
            body.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                i["severity"].as_str().unwrap_or_default(),
                i["stage"].as_str().unwrap_or_default(),
                cell(i["title"].as_str().unwrap_or_default()),
                loc
            ));
        }
        if issues.len() > 20 {
            body.push_str(&format!("\n另有 {} 項，見完整報告。\n", issues.len() - 20));
        }
    }
    body.push_str(&format!("\n[完整報告]({})\n", report_url(s, v)));
    (title, body.chars().take(60_000).collect())
}

/// 驗證開始：建立 in_progress 的 check run，回傳 ID
pub async fn create(s: &AppState, p: &Project, v: &Verification) -> Option<i64> {
    let (auth, repo) = auth(s, p).await?;
    let r = gh_post(
        s,
        &format!("/repos/{repo}/check-runs"),
        &auth,
        json!({
            "name": CHECK_NAME, "head_sha": v.commit_sha, "status": "in_progress",
            "external_id": v.id, "details_url": report_url(s, v),
            "output": { "title": "驗證中", "summary": "Caloud 正在建置、檢查與測試這個版本。" },
        }),
    )
    .await
    .map_err(|e| tracing::warn!(project = %p.id, error = %e, "github check create failed"))
    .ok()?;
    r["id"].as_i64()
}

/// 驗證結束：完成 check run；PR 驗證另更新留言
pub async fn complete(s: &AppState, p: &Project, v: &Verification, issues: &[Value]) {
    let Some((auth, repo)) = auth(s, p).await else { return };
    let (title, body) = summary(s, v, issues);
    if let Some(id) = v.check_run_id {
        if let Err(e) = gh_patch(
            s,
            &format!("/repos/{repo}/check-runs/{id}"),
            &auth,
            json!({
                "status": "completed", "conclusion": conclusion(&v.status),
                "output": { "title": title, "summary": body },
            }),
        )
        .await
        {
            tracing::warn!(project = %p.id, error = %e, "github check complete failed");
        }
    }
    if let Some(pr) = v.pr_number {
        let text = format!(
            "{MARKER}\n### Caloud 驗證：{}\n\nCommit `{}`\n\n{body}",
            title,
            v.commit_sha.chars().take(12).collect::<String>()
        );
        let existing: Option<i64> = sqlx::query_scalar("SELECT comment_id FROM github_pr_comments WHERE project = ? AND pr = ?")
            .bind(&p.id)
            .bind(pr)
            .fetch_optional(&s.db)
            .await
            .ok()
            .flatten();
        let res = match existing {
            Some(cid) => gh_patch(s, &format!("/repos/{repo}/issues/comments/{cid}"), &auth, json!({ "body": text }))
                .await
                .map(|_| cid),
            None => gh_post(s, &format!("/repos/{repo}/issues/{pr}/comments"), &auth, json!({ "body": text }))
                .await
                .and_then(|r| r["id"].as_i64().ok_or_else(|| anyhow::anyhow!("沒有留言 ID"))),
        };
        match res {
            Ok(cid) => {
                let _ = sqlx::query("INSERT OR REPLACE INTO github_pr_comments (project, pr, comment_id) VALUES (?, ?, ?)")
                    .bind(&p.id)
                    .bind(pr)
                    .bind(cid)
                    .execute(&s.db)
                    .await;
            }
            Err(e) => tracing::warn!(project = %p.id, error = %e, "github pr comment failed"),
        }
    }
}

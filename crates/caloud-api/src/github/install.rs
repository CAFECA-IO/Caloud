//! GitHub App 安裝與擁有者確認（FR-GH-01，ADR 0017）。
//!
//! 1. `install_url`：一次性 state（綁定發起的 Caloud 帳號，10 分鐘）
//! 2. GitHub 回呼 `/api/github/callback` → 302 到主控台，由同站的頁面呼叫 `complete`
//!    （session cookie 為 SameSite=Strict，跨站導回時不會帶上）
//! 3. `complete`：確認 session 與 state 是同一個帳號，交換一次性 user token，
//!    以 `GET /user/installations` 確認安裝屬於這個 GitHub 使用者，再綁定
use super::app::{creds, gh_get, installation_token};
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::db::audit;
use caloud_core::now;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Installation {
    pub id: i64,
    #[serde(skip)]
    pub owner: String,
    pub account_login: String,
    pub account_type: String,
    pub suspended: bool,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct Repo {
    pub full_name: String,
    pub private: bool,
    pub default_branch: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteInput {
    pub code: String,
    pub installation_id: i64,
    pub state: String,
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::BadRequest(m.into())
}

/// FR-GH-01：安裝網址（state 一次性、綁定帳號、10 分鐘）
pub async fn install_url(s: &AppState, me: &CurrentUser) -> ApiResult<String> {
    me.require_deploy()?;
    let c = creds(s)
        .await?
        .ok_or_else(|| ApiError::Conflict("GitHub App 尚未設定，請洽管理員。".into()))?;
    let state = caloud_auth::new_token();
    s.github.put_state(state.clone(), me.0.account.clone());
    Ok(format!("{}/apps/{}/installations/new?state={state}", s.cfg.github_web, c.slug))
}

/// FR-GH-01：確認安裝者並綁定
pub async fn complete(s: &AppState, me: &CurrentUser, input: CompleteInput) -> ApiResult<Installation> {
    me.require_deploy()?;
    let account = s
        .github
        .take_state(&input.state)
        .ok_or_else(|| bad("授權已過期或無效，請重新連結 GitHub。"))?;
    if account != me.0.account {
        return Err(ApiError::Forbidden);
    }
    let c = creds(s).await?.ok_or_else(|| ApiError::Conflict("GitHub App 尚未設定。".into()))?;
    // 一次性 user token：只用於確認安裝者，不保存
    let res: Value = s
        .http
        .post(format!("{}/login/oauth/access_token", s.cfg.github_web))
        .header("accept", "application/json")
        .header("user-agent", "Caloud")
        .form(&[
            ("client_id", c.client_id.as_str()),
            ("client_secret", c.client_secret.as_str()),
            ("code", input.code.as_str()),
        ])
        .send()
        .await
        .map_err(|e| bad(format!("無法連線 GitHub：{}", e.without_url())))?
        .json()
        .await
        .map_err(|_| bad("GitHub 回應格式錯誤。"))?;
    let user_token = res["access_token"].as_str().ok_or_else(|| {
        bad(format!(
            "GitHub 授權失敗：{}",
            res["error_description"].as_str().unwrap_or("未知錯誤")
        ))
    })?;
    let list = gh_get(s, "/user/installations?per_page=100", &format!("Bearer {user_token}"))
        .await
        .map_err(|e| bad(e.to_string()))?;
    let inst = list["installations"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["id"].as_i64() == Some(input.installation_id)))
        .ok_or(ApiError::Forbidden)?;
    let login = inst["account"]["login"].as_str().unwrap_or_default().to_string();
    let kind = inst["account"]["type"].as_str().unwrap_or("User").to_string();
    let existing: Option<Installation> = sqlx::query_as("SELECT * FROM github_installations WHERE id = ?")
        .bind(input.installation_id)
        .fetch_optional(&s.db)
        .await?;
    if existing.as_ref().is_some_and(|e| e.owner != me.0.account) {
        return Err(ApiError::Conflict("這個 GitHub 安裝已連結到其他 Caloud 帳號。".into()));
    }
    sqlx::query(
        "INSERT INTO github_installations (id, owner, account_login, account_type, suspended, created_at) VALUES (?, ?, ?, ?, 0, ?)
         ON CONFLICT(id) DO UPDATE SET account_login = excluded.account_login, account_type = excluded.account_type",
    )
    .bind(input.installation_id)
    .bind(&me.0.account)
    .bind(&login)
    .bind(&kind)
    .bind(now())
    .execute(&s.db)
    .await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "github_install",
        Some(&input.installation_id.to_string()),
        Some(&login),
    )
    .await;
    owned(s, me, input.installation_id).await
}

/// 自己的安裝（admin+ 看全部）
pub async fn list(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<Installation>> {
    me.require_deploy()?;
    let q = if me.role().is_admin() {
        sqlx::query_as("SELECT * FROM github_installations ORDER BY account_login")
    } else {
        sqlx::query_as("SELECT * FROM github_installations WHERE owner = ? ORDER BY account_login").bind(&me.0.account)
    };
    Ok(q.fetch_all(&s.db).await?)
}

/// 安裝必須屬於自己（或 admin+）
pub async fn owned(s: &AppState, me: &CurrentUser, id: i64) -> ApiResult<Installation> {
    me.require_deploy()?;
    let i: Installation = sqlx::query_as("SELECT * FROM github_installations WHERE id = ?")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    me.can_manage(&i.owner).then_some(i).ok_or(ApiError::Forbidden)
}

/// 安裝可存取的儲存庫（最多 500 個）
pub async fn repositories(s: &AppState, me: &CurrentUser, id: i64) -> ApiResult<Vec<Repo>> {
    let inst = owned(s, me, id).await?;
    if inst.suspended {
        return Err(ApiError::Conflict("這個 GitHub 安裝已暫停。".into()));
    }
    let token = installation_token(s, id).await.map_err(|e| bad(e.to_string()))?;
    let mut out = Vec::new();
    for page in 1..=5 {
        let v = gh_get(
            s,
            &format!("/installation/repositories?per_page=100&page={page}"),
            &format!("Bearer {token}"),
        )
        .await
        .map_err(|e| bad(e.to_string()))?;
        let repos = v["repositories"].as_array().cloned().unwrap_or_default();
        let n = repos.len();
        out.extend(repos.into_iter().filter_map(|r| {
            Some(Repo {
                full_name: r["full_name"].as_str()?.to_string(),
                private: r["private"].as_bool().unwrap_or(false),
                default_branch: r["default_branch"].as_str().unwrap_or("main").to_string(),
            })
        }));
        if n < 100 {
            break;
        }
    }
    out.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    Ok(out)
}

/// `owner/repo` 格式
pub fn valid_full_name(s: &str) -> bool {
    let mut parts = s.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p.len() <= 100
                && p != "."
                && p != ".."
                && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    #[test]
    fn full_name_rules() {
        assert!(super::valid_full_name("CAFECA-IO/iSunFA"));
        assert!(super::valid_full_name("a/b.c_d"));
        assert!(!super::valid_full_name("a"));
        assert!(!super::valid_full_name("a/b/c"));
        assert!(!super::valid_full_name("a/.."));
        assert!(!super::valid_full_name("a/b?x=1"));
    }
}

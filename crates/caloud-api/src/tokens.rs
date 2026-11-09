//! 個人 API 權杖（FR-MCP-02、FR-MCP-03，ADR 0008）。
use crate::auth::hash_token;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use caloud_core::db::audit;
use caloud_core::model::{ApiToken, User};
use caloud_core::now;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(sqlx::FromRow)]
struct TokenOwner {
    token_id: String,
    token_name: String,
    #[sqlx(flatten)]
    user: User,
}

pub const TOKEN_PREFIX: &str = "cld_";
const DAY: i64 = 86_400;

pub async fn list(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<ApiToken>>> {
    me.require_deploy()?;
    let rows = sqlx::query_as(
        "SELECT id, name, prefix, created_at, expires_at, last_used_at FROM api_tokens WHERE account = ? ORDER BY created_at DESC",
    )
    .bind(&me.0.account)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct CreateToken {
    name: String,
    expires_in_days: Option<i64>,
}

/// 權杖只在此回應出現一次
pub async fn create(State(s): State<AppState>, me: CurrentUser, Json(b): Json<CreateToken>) -> ApiResult<(StatusCode, Json<Value>)> {
    me.require_deploy()?;
    // 只允許從網頁（CAFECA 登入）建立，避免權杖自我繁殖
    if me.1 != "web" {
        return Err(ApiError::Forbidden);
    }
    let name = b.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::BadRequest("請輸入 1 到 60 字的權杖名稱，例如 Claude Desktop。".into()));
    }
    let days = b.expires_in_days.unwrap_or(90);
    if !(1..=365).contains(&days) {
        return Err(ApiError::BadRequest("有效天數需介於 1 到 365 天。".into()));
    }
    let token = format!("{TOKEN_PREFIX}{}", caloud_auth::new_token());
    let id = uuid::Uuid::new_v4().to_string();
    let row: ApiToken = sqlx::query_as(
        "INSERT INTO api_tokens (id, account, name, token_hash, prefix, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)
         RETURNING id, name, prefix, created_at, expires_at, last_used_at",
    )
    .bind(&id)
    .bind(&me.0.account)
    .bind(&name)
    .bind(hash_token(&token))
    .bind(&token[..8])
    .bind(now())
    .bind(now() + days * DAY)
    .fetch_one(&s.db)
    .await?;
    audit(&s.db, Some(&me.0.account), "token_create", Some(&id), Some(&name)).await;
    Ok((StatusCode::CREATED, Json(json!({ "token": token, "api_token": row }))))
}

pub async fn remove(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    me.require_deploy()?;
    let r = sqlx::query("DELETE FROM api_tokens WHERE id = ? AND account = ?")
        .bind(&id)
        .bind(&me.0.account)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit(&s.db, Some(&me.0.account), "token_revoke", Some(&id), None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// FR-MCP-03：驗證 `Authorization: Bearer cld_…`；權限為使用者目前角色，pending／disabled 視為無效
pub async fn authenticate(s: &AppState, authorization: Option<&str>) -> ApiResult<CurrentUser> {
    let token = authorization
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| t.starts_with(TOKEN_PREFIX))
        .ok_or(ApiError::Unauthorized)?;
    let row: Option<TokenOwner> = sqlx::query_as(
        "SELECT t.id AS token_id, t.name AS token_name, u.* FROM api_tokens t
         JOIN users u ON u.account = t.account
         WHERE t.token_hash = ? AND t.expires_at > ?",
    )
    .bind(hash_token(token))
    .bind(now())
    .fetch_optional(&s.db)
    .await?;
    let TokenOwner {
        token_id,
        token_name,
        user,
    } = row.ok_or(ApiError::Unauthorized)?;
    if user.role == "pending" || user.role == "disabled" {
        return Err(ApiError::Unauthorized);
    }
    // 最後使用時間只供顯示，寫入失敗不影響驗證
    if let Err(e) = sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
        .bind(now())
        .bind(&token_id)
        .execute(&s.db)
        .await
    {
        tracing::warn!(error = %e, "update token last_used_at failed");
    }
    Ok(CurrentUser(user, format!("mcp:{token_name}")))
}

use crate::error::{ApiError, ApiResult};
use crate::AppState;
use alloy_primitives::keccak256;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use caloud_auth::SignInResponse;
use caloud_core::model::{Role, User};
use caloud_core::now;
use serde_json::{json, Value};

const COOKIE: &str = "caloud_session";
const SESSION_TTL: i64 = 7 * 24 * 3600;
const NONCE_TTL: i64 = 5 * 60;

pub(crate) fn hash_token(t: &str) -> String {
    hex::encode(keccak256(t.as_bytes()))
}

/// 前端初始化 CafecaConnect 用
pub async fn config(State(s): State<AppState>) -> Json<Value> {
    Json(json!({ "wallet": s.verifier.wallet(), "domain": s.cfg.public_origin }))
}

pub async fn nonce(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    let n = caloud_auth::new_nonce();
    sqlx::query("DELETE FROM login_nonces WHERE expires_at < ?")
        .bind(now())
        .execute(&s.db)
        .await?;
    sqlx::query("INSERT INTO login_nonces (nonce, expires_at) VALUES (?, ?)")
        .bind(&n)
        .bind(now() + NONCE_TTL)
        .execute(&s.db)
        .await?;
    Ok(Json(json!({ "nonce": n })))
}

pub async fn login(State(s): State<AppState>, jar: CookieJar, Json(res): Json<SignInResponse>) -> ApiResult<(CookieJar, Json<User>)> {
    let nonce = res
        .message
        .as_ref()
        .map(|m| m.nonce.clone())
        .ok_or_else(|| ApiError::BadRequest("缺少 message".into()))?;
    // FR-AUTH-02：不論成功與否都作廢，一個 nonce 只能用一次
    let valid: Option<i64> = sqlx::query_scalar("DELETE FROM login_nonces WHERE nonce = ? RETURNING expires_at")
        .bind(&nonce)
        .fetch_optional(&s.db)
        .await?;
    if !valid.is_some_and(|exp| exp >= now()) {
        return Err(ApiError::BadRequest("nonce 無效或已過期".into()));
    }

    let v = s
        .verifier
        .verify(&res, &s.cfg.public_origin, &nonce, now() as u64)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    // FR-AUTH-03／04：第一位登入者為 super_admin，其餘為預設角色
    let default_role = Role::parse(&s.cfg.default_role).unwrap_or(Role::Pending);
    let user = caloud_core::db::upsert_login(&s.db, &v.account, v.handle.as_deref(), default_role).await?;
    if user.role == "disabled" {
        return Err(ApiError::Forbidden);
    }

    // FR-AUTH-05：DB 只存 token 雜湊
    let token = caloud_auth::new_token();
    sqlx::query("INSERT INTO sessions (token_hash, account, created_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(hash_token(&token))
        .bind(&user.account)
        .bind(now())
        .bind(now() + SESSION_TTL)
        .execute(&s.db)
        .await?;
    caloud_core::db::audit(&s.db, Some(&user.account), "user_login", None, Some(&user.role)).await;

    let cookie = Cookie::build((COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(s.cfg.public_origin.starts_with("https://"))
        .same_site(SameSite::Strict)
        .max_age(time_dur(SESSION_TTL))
        .build();
    Ok((jar.add(cookie), Json(user)))
}

fn time_dur(secs: i64) -> time::Duration {
    time::Duration::seconds(secs)
}

/// FR-AUTH-06
pub async fn logout(State(s): State<AppState>, jar: CookieJar) -> ApiResult<CookieJar> {
    if let Some(c) = jar.get(COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(hash_token(c.value()))
            .execute(&s.db)
            .await?;
    }
    Ok(jar.remove(Cookie::build(COOKIE).path("/")))
}

pub async fn me(user: CurrentUser) -> Json<User> {
    Json(user.0)
}

/// 已登入的使用者與操作來源：`web`（session cookie）或 `mcp:<權杖名稱>`（API 權杖）
pub struct CurrentUser(pub User, pub String);

impl CurrentUser {
    /// FR-MCP-07：稽核細節附上來源；網頁操作維持原樣
    pub fn audit_detail(&self, extra: Option<&str>) -> Option<String> {
        if self.1 == "web" {
            return extra.map(str::to_string);
        }
        Some(match extra {
            Some(x) => format!("via={}; {x}", self.1),
            None => format!("via={}", self.1),
        })
    }
    pub fn role(&self) -> Role {
        Role::parse(&self.0.role).unwrap_or(Role::Disabled)
    }
    pub fn require_deploy(&self) -> ApiResult<()> {
        self.role().can_deploy().then_some(()).ok_or(ApiError::Forbidden)
    }
    pub fn require_admin(&self) -> ApiResult<()> {
        self.role().is_admin().then_some(()).ok_or(ApiError::Forbidden)
    }
    /// 資源擁有者或 admin
    pub fn can_manage(&self, owner: &str) -> bool {
        self.0.account == owner || self.role().is_admin()
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(COOKIE).ok_or(ApiError::Unauthorized)?;
        let user: Option<User> =
            sqlx::query_as("SELECT u.* FROM sessions s JOIN users u ON u.account = s.account WHERE s.token_hash = ? AND s.expires_at > ?")
                .bind(hash_token(token.value()))
                .bind(now())
                .fetch_optional(&s.db)
                .await?;
        match user {
            Some(u) if u.role != "disabled" => Ok(CurrentUser(u, "web".into())),
            _ => Err(ApiError::Unauthorized),
        }
    }
}

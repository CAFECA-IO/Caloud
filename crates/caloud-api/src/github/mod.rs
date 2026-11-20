//! GitHub App 整合（M5-01，[ADR 0017](../../../../docs/adr/0017-github-app-public-console.md)）。
//!
//! - [`app`]：後台設定的憑證（FR-GH-05）、App JWT、installation token
//! - [`install`]：安裝與擁有者確認（FR-GH-01）
//! - [`webhook`]：驗簽、去重、事件分派（FR-GH-02）
pub mod app;
pub mod checks;
pub mod install;
pub mod webhook;

use crate::error::ApiResult;
use crate::{AppState, CurrentUser};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use caloud_core::now;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// 一次性 state 的有效時間
const STATE_TTL: i64 = 600;

/// 只在記憶體的狀態：安裝 state 與 installation token（不寫入資料庫，ADR 0017）
#[derive(Default)]
pub struct Runtime {
    /// state → (Caloud 帳號, 到期)
    states: Mutex<HashMap<String, (String, i64)>>,
    /// installation id → (token, 使用期限)
    tokens: Mutex<HashMap<i64, (String, i64)>>,
}

impl Runtime {
    pub(crate) fn put_state(&self, state: String, account: String) {
        let t = now();
        let mut m = self.states.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (_, exp)| *exp > t);
        m.insert(state, (account, t + STATE_TTL));
    }

    /// 一次性：取出即作廢
    pub(crate) fn take_state(&self, state: &str) -> Option<String> {
        let mut m = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let (acct, exp) = m.remove(state)?;
        (exp > now()).then_some(acct)
    }

    pub(crate) fn cached_token(&self, id: i64) -> Option<String> {
        let m = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        m.get(&id).filter(|(_, until)| *until > now()).map(|(t, _)| t.clone())
    }

    pub(crate) fn store_token(&self, id: i64, token: String, until: i64) {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).insert(id, (token, until));
    }

    pub(crate) fn forget_token(&self, id: i64) {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }

    pub(crate) fn clear_tokens(&self) {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

// ───────────────────────── REST（docs/06 §3.8） ─────────────────────────

pub async fn admin_get(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<app::AppInfo>> {
    Ok(Json(app::info(&s, &me).await?))
}

pub async fn admin_put(State(s): State<AppState>, me: CurrentUser, Json(b): Json<app::AppInput>) -> ApiResult<Json<app::AppInfo>> {
    Ok(Json(app::set(&s, &me, b).await?))
}

pub async fn install_url(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "url": install::install_url(&s, &me).await? })))
}

/// GitHub 導回：session cookie（SameSite=Strict）在跨站導回時不會帶上，
/// 因此不在這裡綁定，而是轉回主控台，由同站頁面呼叫 `POST /api/github/complete`
pub async fn callback(Query(q): Query<HashMap<String, String>>) -> Redirect {
    let keep: Vec<String> = ["code", "installation_id", "state", "setup_action"]
        .iter()
        .filter_map(|k| {
            let v = q.get(*k)?;
            v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .then(|| format!("{k}={v}"))
        })
        .collect();
    Redirect::to(&format!("/#github-complete/{}", keep.join("&")))
}

pub async fn complete(
    State(s): State<AppState>,
    me: CurrentUser,
    Json(b): Json<install::CompleteInput>,
) -> ApiResult<Json<install::Installation>> {
    Ok(Json(install::complete(&s, &me, b).await?))
}

pub async fn installations(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<install::Installation>>> {
    Ok(Json(install::list(&s, &me).await?))
}

pub async fn repositories(State(s): State<AppState>, me: CurrentUser, Path(id): Path<i64>) -> ApiResult<Json<Vec<install::Repo>>> {
    Ok(Json(install::repositories(&s, &me, id).await?))
}

pub async fn webhook(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let (st, msg) = webhook::handle(&s, &headers, &body).await;
    (st, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], msg).into_response()
}

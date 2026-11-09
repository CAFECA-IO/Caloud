//! 使用者 REST handler：只做 HTTP 轉換，邏輯在 `service`（FR-USER-*）。
use crate::error::ApiResult;
use crate::{service, AppState, CurrentUser};
use axum::extract::{Path, State};
use axum::Json;
use caloud_core::model::{Role, User};
use serde::Deserialize;

pub async fn list(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<User>>> {
    Ok(Json(service::list_users(&s, &me).await?))
}

#[derive(Deserialize)]
pub struct SetRole {
    role: Role,
}

pub async fn set_role(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(account): Path<String>,
    Json(b): Json<SetRole>,
) -> ApiResult<Json<User>> {
    Ok(Json(service::set_user_role(&s, &me, &account, b.role).await?))
}

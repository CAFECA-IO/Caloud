//! 專案 REST handler：只做 HTTP 轉換，邏輯在 `service::projects`（docs/06 §3.7）。
use crate::error::ApiResult;
use crate::service::projects::{self, ConfirmInput, ProjectInput};
use crate::{AppState, CurrentUser};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use caloud_core::model::Project;
use serde_json::Value;

pub async fn list(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<Project>>> {
    Ok(Json(projects::list(&s, &me).await?))
}

pub async fn create(State(s): State<AppState>, me: CurrentUser, Json(b): Json<ProjectInput>) -> ApiResult<(StatusCode, Json<Project>)> {
    Ok((StatusCode::ACCEPTED, Json(projects::create(&s, &me, b).await?)))
}

pub async fn get(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(projects::get(&s, &me, &id).await?))
}

pub async fn update(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    Json(b): Json<ProjectInput>,
) -> ApiResult<Json<Project>> {
    Ok(Json(projects::update(&s, &me, &id, b).await?))
}

pub async fn remove(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    projects::delete(&s, &me, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reanalyze(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<(StatusCode, Json<Project>)> {
    Ok((StatusCode::ACCEPTED, Json(projects::reanalyze(&s, &me, &id).await?)))
}

pub async fn confirm(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    Json(b): Json<ConfirmInput>,
) -> ApiResult<Json<Project>> {
    Ok(Json(projects::confirm(&s, &me, &id, b).await?))
}

/// FR-ENV-12：部署到 production（ADR 0018）
pub async fn deploy(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<(StatusCode, Json<Value>)> {
    Ok((StatusCode::ACCEPTED, Json(crate::service::deploy::deploy(&s, &me, &id).await?)))
}

//! App REST handler：只做 HTTP 轉換，邏輯在 `service`（FR-APP-*）。
use crate::error::ApiResult;
use crate::{service, AppState, CurrentUser};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use caloud_core::model::App;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct UpsertApp {
    name: Option<String>,
    compose_yaml: String,
    /// 建立或更新後立即部署（預設 true）
    #[serde(default = "yes")]
    deploy: bool,
}
fn yes() -> bool {
    true
}

pub async fn list(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<App>>> {
    Ok(Json(service::list_apps(&s, &me).await?))
}

pub async fn create(State(s): State<AppState>, me: CurrentUser, Json(b): Json<UpsertApp>) -> ApiResult<(StatusCode, Json<App>)> {
    let app = service::create_app(&s, &me, b.name, &b.compose_yaml, b.deploy).await?;
    Ok((StatusCode::CREATED, Json(app)))
}

pub async fn get(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(service::get_app(&s, &me, &id).await?))
}

pub async fn update(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>, Json(b): Json<UpsertApp>) -> ApiResult<Json<App>> {
    Ok(Json(service::update_app(&s, &me, &id, b.name, &b.compose_yaml, b.deploy).await?))
}

pub async fn deploy(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    service::deploy_app(&s, &me, &id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn stop(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    service::stop_app(&s, &me, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct RemoveQuery {
    #[serde(default)]
    purge_volumes: bool,
}

pub async fn remove(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<StatusCode> {
    service::delete_app(&s, &me, &id, q.purge_volumes).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct LogsQuery {
    service: Option<String>,
    tail: Option<u32>,
}

pub async fn logs(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>, Query(q): Query<LogsQuery>) -> ApiResult<String> {
    service::app_logs(&s, &me, &id, q.service.as_deref(), q.tail).await
}

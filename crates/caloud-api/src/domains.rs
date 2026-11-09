//! 網域 REST handler：只做 HTTP 轉換，邏輯在 `service`（FR-DOM-*）。
use crate::error::ApiResult;
use crate::{service, AppState, CurrentUser};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use caloud_core::model::Domain;
use serde::Deserialize;

pub async fn list(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<Domain>>> {
    Ok(Json(service::list_domains(&s, &me).await?))
}

#[derive(Deserialize)]
pub struct CreateDomain {
    hostname: String,
}

pub async fn create(State(s): State<AppState>, me: CurrentUser, Json(b): Json<CreateDomain>) -> ApiResult<(StatusCode, Json<Domain>)> {
    Ok((StatusCode::CREATED, Json(service::create_domain(&s, &me, &b.hostname).await?)))
}

pub async fn verify(State(s): State<AppState>, me: CurrentUser, Path(hostname): Path<String>) -> ApiResult<Json<Domain>> {
    Ok(Json(service::verify_domain(&s, &me, &hostname).await?))
}

#[derive(Deserialize)]
pub struct Bind {
    app_id: Option<String>,
    service: Option<String>,
    port: Option<u16>,
}

pub async fn bind(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(hostname): Path<String>,
    Json(b): Json<Bind>,
) -> ApiResult<Json<Domain>> {
    let d = service::bind_domain(&s, &me, &hostname, b.app_id.as_deref(), b.service.as_deref(), b.port).await?;
    Ok(Json(d))
}

pub async fn remove(State(s): State<AppState>, me: CurrentUser, Path(hostname): Path<String>) -> ApiResult<StatusCode> {
    service::delete_domain(&s, &me, &hostname).await?;
    Ok(StatusCode::NO_CONTENT)
}

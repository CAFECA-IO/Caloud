//! 後台 Admin API（docs/06 §3.6）：只做請求解析，業務邏輯在 `service::admin`。
use crate::error::ApiResult;
use crate::service::admin::{self as svc, BlockedHost, DomainPool, Node, PoolInput, ReservedName};
use crate::{AppState, CurrentUser};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use caloud_core::placement::NodeStatus;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn list_pools(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<DomainPool>>> {
    Ok(Json(svc::list_pools(&s, &me).await?))
}

pub async fn create_pool(
    State(s): State<AppState>,
    me: CurrentUser,
    Json(b): Json<PoolInput>,
) -> ApiResult<(StatusCode, Json<DomainPool>)> {
    Ok((StatusCode::CREATED, Json(svc::create_pool(&s, &me, b).await?)))
}

pub async fn update_pool(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(root): Path<String>,
    Json(b): Json<PoolInput>,
) -> ApiResult<Json<DomainPool>> {
    Ok(Json(svc::update_pool(&s, &me, &root, b).await?))
}

pub async fn delete_pool(State(s): State<AppState>, me: CurrentUser, Path(root): Path<String>) -> ApiResult<StatusCode> {
    svc::delete_pool(&s, &me, &root).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct NameBody {
    name: String,
}

pub async fn list_reserved(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<ReservedName>>> {
    Ok(Json(svc::list_reserved(&s, &me).await?))
}

pub async fn add_reserved(
    State(s): State<AppState>,
    me: CurrentUser,
    Json(b): Json<NameBody>,
) -> ApiResult<(StatusCode, Json<ReservedName>)> {
    Ok((StatusCode::CREATED, Json(svc::add_reserved(&s, &me, &b.name).await?)))
}

pub async fn remove_reserved(State(s): State<AppState>, me: CurrentUser, Path(name): Path<String>) -> ApiResult<StatusCode> {
    svc::remove_reserved(&s, &me, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct BlockBody {
    hostname: String,
    reason: String,
}

pub async fn list_blocked(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<BlockedHost>>> {
    Ok(Json(svc::list_blocked(&s, &me).await?))
}

pub async fn block_host(
    State(s): State<AppState>,
    me: CurrentUser,
    Json(b): Json<BlockBody>,
) -> ApiResult<(StatusCode, Json<BlockedHost>)> {
    Ok((StatusCode::CREATED, Json(svc::block_host(&s, &me, &b.hostname, &b.reason).await?)))
}

pub async fn unblock_host(State(s): State<AppState>, me: CurrentUser, Path(hostname): Path<String>) -> ApiResult<StatusCode> {
    svc::unblock_host(&s, &me, &hostname).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct KindBody {
    kind: String,
}

pub async fn subdomain_id(State(s): State<AppState>, me: CurrentUser, Json(b): Json<KindBody>) -> ApiResult<Json<Value>> {
    let (id, hostname) = svc::preview_env_id(&s, &me, &b.kind).await?;
    Ok(Json(json!({ "id": id, "hostname": hostname })))
}

pub async fn list_nodes(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<Node>>> {
    Ok(Json(svc::list_nodes(&s, &me).await?))
}

#[derive(Deserialize, Default)]
pub struct JoinBody {
    expires_in_minutes: Option<i64>,
}

pub async fn join_token(State(s): State<AppState>, me: CurrentUser, body: Option<Json<JoinBody>>) -> ApiResult<(StatusCode, Json<Value>)> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let (token, expires_at) = svc::create_join_token(&s, &me, b.expires_in_minutes).await?;
    Ok((StatusCode::CREATED, Json(json!({ "token": token, "expires_at": expires_at }))))
}

pub async fn drain_node(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Node>> {
    Ok(Json(svc::set_node_status(&s, &me, &id, NodeStatus::Draining).await?))
}

pub async fn activate_node(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Node>> {
    Ok(Json(svc::set_node_status(&s, &me, &id, NodeStatus::Active).await?))
}

pub async fn delete_node(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    svc::delete_node(&s, &me, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

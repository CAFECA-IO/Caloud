//! 專案 REST handler：只做 HTTP 轉換，邏輯在 `service::projects`（docs/06 §3.7）。
use crate::error::ApiResult;
use crate::service::projects::{self, ConfirmInput, ProjectInput};
use crate::{AppState, CurrentUser};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use caloud_core::model::Project;
use serde_json::Value;

/// 列表另含最近一次驗證的狀態（FR-RPT-04 站內通知）
pub async fn list(State(s): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<Value>>> {
    let list = projects::list(&s, &me).await?;
    let mut out = Vec::with_capacity(list.len());
    for p in list {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM verifications WHERE project = ? ORDER BY created_at DESC, rowid DESC LIMIT 1")
                .bind(&p.id)
                .fetch_optional(&s.db)
                .await
                .map_err(crate::error::ApiError::from)?;
        let mut v = serde_json::to_value(&p).unwrap_or_default();
        v["verify_status"] = status.into();
        out.push(v);
    }
    Ok(Json(out))
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
pub async fn deploy(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let input: crate::service::deploy::DeployInput = if body.is_empty() {
        Default::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| crate::error::ApiError::BadRequest(format!("格式錯誤：{e}")))?
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::service::deploy::deploy(&s, &me, &id, input).await?),
    ))
}

#[derive(serde::Deserialize, Default)]
pub struct VerifyInput {
    #[serde(default)]
    pub keep_on_failure: bool,
}

/// FR-CI-01：手動驗證（ADR 0020）
pub async fn verify(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<crate::service::verify::Verification>)> {
    let input: VerifyInput = if body.is_empty() {
        Default::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| crate::error::ApiError::BadRequest(format!("格式錯誤：{e}")))?
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::service::verify::manual(&s, &me, &id, input.keep_on_failure).await?),
    ))
}

pub async fn verifications(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<crate::service::verify::Verification>>> {
    Ok(Json(crate::service::verify::list(&s, &me, &id).await?))
}

/// ADR 0023
pub async fn deployments(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<crate::service::deploy::DeploymentRecord>>> {
    Ok(Json(crate::service::deploy::history(&s, &me, &id).await?))
}

#[derive(serde::Deserialize)]
pub struct VolumeQuery {
    pub service: String,
    pub target: String,
}

/// ADR 0024：加上 volume 的預覽
pub async fn volume_preview(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<VolumeQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(crate::service::volume::preview(&s, &me, &id, &q.service, &q.target).await?))
}

/// ADR 0024：加上 volume
pub async fn volume_apply(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    Json(b): Json<crate::service::volume::VolumeInput>,
) -> ApiResult<Json<Value>> {
    Ok(Json(crate::service::volume::apply(&s, &me, &id, b).await?))
}

pub async fn verification(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(crate::service::verify::detail(&s, &me, &id).await?))
}

pub async fn cancel_verification(State(s): State<AppState>, me: CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    crate::service::verify::cancel(&s, &me, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub struct ReportQuery {
    #[serde(default)]
    pub format: Option<String>,
}

pub async fn report(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ReportQuery>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let md = q.format.as_deref() == Some("md");
    let (body, ctype, ext) = if md {
        (
            crate::service::verify::report_md(&s, &me, &id).await?,
            "text/markdown; charset=utf-8",
            "md",
        )
    } else {
        (
            serde_json::to_string_pretty(&crate::service::verify::detail(&s, &me, &id).await?).unwrap_or_default(),
            "application/json",
            "json",
        )
    };
    let short: String = id.chars().take(8).collect();
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, ctype.to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"caloud-verification-{short}.{ext}\""),
            ),
        ],
        body,
    )
        .into_response())
}

#[derive(serde::Deserialize)]
pub struct EnvQuery {
    #[serde(default)]
    pub env: Option<String>,
}

/// FR-ENV-05：秘密變數（只回名稱與提示，ADR 0019）
pub async fn vars(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<EnvQuery>,
) -> ApiResult<Json<crate::service::vars::VarList>> {
    let env = q.env.unwrap_or_else(|| "production".into());
    Ok(Json(crate::service::vars::list(&s, &me, &id, &env).await?))
}

pub async fn set_vars(
    State(s): State<AppState>,
    me: CurrentUser,
    Path(id): Path<String>,
    Json(b): Json<crate::service::vars::VarsInput>,
) -> ApiResult<Json<crate::service::vars::VarList>> {
    Ok(Json(crate::service::vars::update(&s, &me, &id, b).await?))
}

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("請先登入")]
    Unauthorized,
    #[error("權限不足")]
    Forbidden,
    #[error("找不到資源")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl ApiError {
    /// 給使用者看的訊息；500 固定為「伺服器錯誤」並把細節寫入 log（NFR-SEC-03）
    pub fn public_message(&self) -> String {
        match self {
            ApiError::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                "伺服器錯誤，請稍後再試。".to_string()
            }
            other => other.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let msg = self.public_message();
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

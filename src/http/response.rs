//! Turning a [`crate::error::ApiError`] into a status code and a JSON body.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::error::ApiError;

// The only place an internal error becomes an HTTP status. Internal deliberately renders a
// fixed message so a cause never leaks to a caller.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::ServiceUnavailable(_) | ApiError::SwishRejected { .. } => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &self {
            ApiError::Internal(_) => "Internal server error.".to_string(),
            other => other.to_string(),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

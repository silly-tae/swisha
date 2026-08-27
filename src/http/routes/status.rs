//! `GET /swish/status/{reference}`.

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde_json::{Value, json};

use crate::store::PayoutStore;
use crate::{domain::errors, error::ApiError, state::SharedState};

// Lets a caller that missed a live notification recover the current state, since pg_notify
// has no replay. The response is shaped exactly like the notification payload so a
// reconnecting client can feed it through the same handler.
/// `GET /swish/status/{reference}`. The current state of one payout, shaped exactly like an
/// `updates` event.
pub async fn status(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(reference): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !crate::http::secret::authorized(&headers, state.config.api_secret.as_deref()) {
        return Err(ApiError::Unauthorized);
    }

    let view = state
        .store
        .status_view(&reference)
        .await
        .map_err(|e| {
            tracing::warn!("[swish/status] store error for {reference}: {e}");
            ApiError::ServiceUnavailable("Could not read the payout.".into())
        })?
        .ok_or(ApiError::NotFound)?;

    let (status, swish_ref, error_code) = (view.status, view.swish_ref, view.error_code);
    let status = status.unwrap_or_default();

    let info = errors::describe_failure(&status, error_code.as_deref());

    Ok(Json(json!({
        "reference":     reference,
        "status":         status,
        "swish_ref":      swish_ref,
        "error_code":     error_code,
        "error_message":  info.map(|i| i.message(state.config.error_language)),
        "error_category": info.map(|i| i.category.as_str()),
    })))
}

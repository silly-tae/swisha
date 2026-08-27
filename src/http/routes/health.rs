//! `GET /system/health`.

use axum::{extract::State, http::HeaderMap, Json};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Duration;

use crate::store::PayoutStore;
use crate::{error::ApiError, state::{SharedState, SwishProbe}};

// Asks Swish whether it is answering. The only slow part of a health check, and the only part
// worth caching.
/// Asks Swish whether it is answering. Cached by the caller; the only slow part of a health
/// check.
pub async fn probe_swish(state: &SharedState) -> SwishProbe {
    let url = format!("{}/swish-cpcapi/api/v1/payouts/ping", state.config.swish_base_url);
    let answered = tokio::time::timeout(
        Duration::from_secs(5),
        state.swish_client.get(&url).send(),
    )
    .await;

    SwishProbe {
        reachable: matches!(answered, Ok(Ok(_))),
        at: SystemTime::now(),
    }
}

/// `GET /system/health`. Pings the database live, and reports the cached Swish probe.
pub async fn get(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    if !crate::http::secret::authorized(&headers, state.config.api_secret.as_deref()) {
        return Err(ApiError::Unauthorized);
    }

    // The database is pinged on every request. It is local and costs about a millisecond, and a
    // health check reporting a database as reachable while it is down is the one answer that
    // makes the endpoint worse than not having one.
    let db_ok = state.store.ping().await.is_ok();
    let probe = *state.swish_probe.read().await;

    let swish_ok = probe.map(|p| p.reachable);
    let checked_ago = probe.and_then(|p| p.at.elapsed().ok()).map(|d| d.as_secs());

    // Swish being unreachable is degraded too: a payout cannot complete without them. An
    // unknown answer is not treated as a failure, because it only means nothing has asked yet.
    let degraded = !db_ok || swish_ok == Some(false);

    Ok(Json(json!({
        "status":       if degraded { "degraded" } else { "ok" },
        "db":           db_ok,
        "swish_online": swish_ok,
        "swish_checked_seconds_ago": checked_ago,
        "started_at":   state.started_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        "timestamp":    crate::util::time::now().rfc3339(),
        "version":      env!("CARGO_PKG_VERSION"),
    })))
}

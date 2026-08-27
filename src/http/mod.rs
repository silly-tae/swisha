//! The HTTP layer: two listeners, deliberately separate.
//!
//! [`internal_router`] carries everything the operator's own backend calls. [`callback_router`]
//! carries the one route Swish itself calls, and nothing else, so a proxy misconfiguration
//! cannot expose the payout endpoint to the internet.

pub mod client_ip;
pub mod rate_limit;
pub mod response;
pub mod routes;
pub mod secret;

use axum::{
    Router,
    routing::{get, post},
};

use crate::state::SharedState;

/// Routes only the operator's own backend may reach: payout, status, events and health.
///
/// Kept off the callback listener so no proxy misconfiguration can expose the payout endpoint.
pub fn internal_router() -> Router<SharedState> {
    Router::new()
        .route("/system/health", get(routes::health::get))
        .route("/swish/payout", post(routes::payout::payout))
        .route("/swish/status/{reference}", get(routes::status::status))
        .route("/events", get(routes::events::events))
        .fallback(not_found)
}

/// The one route Swish itself calls, and so the only one that has to be publicly routable.
pub fn callback_router() -> Router<SharedState> {
    Router::new()
        .route("/swish/callback", post(routes::callback::callback))
        .fallback(not_found)
}

// Each listener owns its whole surface, unknown paths included, so a routing test exercises
// what actually ships rather than a router main.rs then adds to.
async fn not_found() -> crate::error::ApiError {
    crate::error::ApiError::NotFound
}

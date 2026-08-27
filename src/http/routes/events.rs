//! `GET /events`, the server-sent event stream.

use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
};
use serde::Deserialize;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::error::ApiError;
use crate::events::StreamEvent;
use crate::http::secret;
use crate::state::SharedState;

#[derive(Debug, Deserialize)]
/// Query parameters for the event stream. Both are optional.
pub struct EventFilter {
    // Suffix of a channel: "events", "updates" or "logs". Omit for everything.
    /// Channel suffix: `events`, `updates` or `logs`. Omit for everything.
    pub channel: Option<String>,
    // Only events for one payout. Entries with no reference, such as service logs, are
    // excluded when this is set.
    /// Only events for one payout. Entries with no reference, such as service logs, are
    /// excluded when this is set.
    pub reference: Option<String>,
}

// Live event stream, fed from an in-process broadcast rather than the database, so it works on
// every backend. Lossy: a subscriber that falls behind misses messages instead of slowing
// payouts. Anything that must not be missed is read back with GET /swish/status/{reference}.
/// `GET /events`. A server-sent event stream, lossy by design.
pub async fn events(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(filter): Query<EventFilter>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    if !secret::authorized(&headers, state.config.api_secret.as_deref()) {
        return Err(ApiError::Unauthorized);
    }

    let prefix = state.config.notify_prefix.clone();
    let wanted_channel = filter.channel.map(|name| format!("{prefix}:{name}"));
    let wanted_reference = filter.reference;

    let stream = BroadcastStream::new(state.stream.subscribe()).filter_map(move |event| {
        // A lagged subscriber is dropped messages, not a broken stream: keep going.
        let event = event.ok()?;

        if !matches(&event, wanted_channel.as_deref(), wanted_reference.as_deref()) {
            return None;
        }

        // The SSE event name is the channel suffix, so a client can switch on it directly.
        let name = event
            .channel
            .rsplit_once(':')
            .map(|(_, suffix)| suffix)
            .unwrap_or(&event.channel);
        Some(Ok(Event::default().event(name).data(event.payload)))
    });

    // Proxies drop idle connections; a periodic comment keeps the stream alive.
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// Both filters are conjunctive, and an event with no reference never matches a reference
// filter: a client watching one payout should not receive unrelated service logs.
/// Whether one event passes the requested filters.
pub fn matches(event: &StreamEvent, channel: Option<&str>, reference: Option<&str>) -> bool {
    if let Some(channel) = channel
        && event.channel != channel
    {
        return false;
    }
    if let Some(reference) = reference
        && event.reference.as_deref() != Some(reference)
    {
        return false;
    }
    true
}

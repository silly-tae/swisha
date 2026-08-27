//! Announcing payout changes, to subscribers here and to listeners elsewhere.
//!
//! Three channels, namespaced by `NOTIFY_PREFIX`: `updates` carries status changes, `events`
//! carries the audit trail, `logs` carries service log lines. `events` and `logs` include
//! amounts and personal data; `updates` is the one safe to forward to a browser.

use tokio::sync::broadcast;
use crate::domain::payout::EventRecord;
use crate::state::SharedState;
use crate::notify::Notifier;
use crate::store::PayoutStore;

/// One event as it reaches a subscriber.
///
/// Carried in process rather than read back from the database, so streaming works on every
/// backend, including one with no notifier.
#[derive(Clone, Debug)]
pub struct StreamEvent {
    /// The full channel name, prefix included.
    pub channel: String,
    /// The payout this concerns, when it concerns one. Service logs carry none.
    pub reference: Option<String>,
    /// The JSON body, already serialized.
    pub payload: String,
}

/// Fan-out to in-process subscribers.
///
/// Lossy by design: a subscriber that falls behind misses messages rather than slowing the
/// payout path. Recovery is `GET /swish/status/{reference}`, never the stream.
#[derive(Clone)]
pub struct EventStream(broadcast::Sender<StreamEvent>);

impl EventStream {
    /// A stream buffering `capacity` messages per subscriber.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self(sender)
    }

    /// A new subscription. Messages published before this call are not replayed.
    pub fn subscribe(&self) -> broadcast::Receiver<StreamEvent> {
        self.0.subscribe()
    }

    /// How many subscribers are currently attached.
    pub fn subscriber_count(&self) -> usize {
        self.0.receiver_count()
    }

    /// Publishes directly, exposed so the lossy behavior can be tested from outside the crate.
    pub fn publish_for_test(&self, event: StreamEvent) {
        self.send(event);
    }

    fn send(&self, event: StreamEvent) {
        // An error means nobody is listening, which is not a failure.
        let _ = self.0.send(event);
    }
}

impl Default for EventStream {
    fn default() -> Self {
        Self::new(512)
    }
}

/// The audit trail channel. Carries amounts and the recipient's number.
///
/// Named from `NOTIFY_PREFIX` so two services can share a database without colliding.
pub fn channel_events(prefix: &str) -> String {
    format!("{prefix}:events")
}

/// The status change channel. Carries no personal data, so it is the one to forward onward.
pub fn channel_updates(prefix: &str) -> String {
    format!("{prefix}:updates")
}

/// The service log channel.
pub fn channel_logs(prefix: &str) -> String {
    format!("{prefix}:logs")
}

/// What one audit trail entry records.
pub struct SwishLogFields {
    /// The payout's reference.
    pub reference:    String,
    /// The `payoutInstructionUUID`, when one exists yet.
    pub swish_ref:     Option<String>,
    /// The status at the time.
    pub status:        Option<String>,
    /// Amount in SEK.
    pub amount:        Option<f64>,
    /// The recipient's Swish number.
    pub payee_alias: Option<String>,
    /// The Swish error code, on failures.
    pub error_code:    Option<String>,
    /// The error text, on failures.
    pub error_message: Option<String>,
    /// The address the request came from.
    pub ip:            Option<String>,
}

/// Records one payout event and publishes it on the events channel.
///
/// Carries the amount and the recipient's number, so subscribers to this channel see personal
/// data.
pub async fn log_swish_event(event: &str, fields: SwishLogFields, state: &SharedState) {
    let payload_str = serde_json::json!({
        "reference":    &fields.reference,
        "swish_ref":     &fields.swish_ref,
        "event":         event,
        "status":        &fields.status,
        "amount":        fields.amount,
        "payee_alias": &fields.payee_alias,
        "error_code":    &fields.error_code,
        "error_message": &fields.error_message,
        "ip":            &fields.ip,
        "created_at":    crate::util::time::now().date_time(),
    })
    .to_string();

    let record = EventRecord {
        reference:     fields.reference.clone(),
        swish_ref:     fields.swish_ref.clone(),
        event:         event.to_string(),
        status:        fields.status.clone(),
        amount:        fields.amount,
        payee_alias:   fields.payee_alias.clone(),
        error_code:    fields.error_code.clone(),
        error_message: fields.error_message.clone(),
        ip:            fields.ip.clone(),
    };

    let channel = channel_events(&state.config.notify_prefix);
    tokio::join!(
        async {
            if let Err(e) = state.store.record_event(&record).await {
                tracing::error!("[events] failed to record '{event}' for {}: {e}", fields.reference);
            }
        },
        publish(state, &channel, Some(&fields.reference), &payload_str),
    );
}

/// Records one service log line and publishes it on the logs channel.
pub async fn log_admin_event(
    level:   &str,
    message: &str,
    context: serde_json::Value,
    ip:      &str,
    state:   &SharedState,
) {
    let ctx_str = if context.is_null() { None } else { Some(context.to_string()) };
    let ip_opt  = if ip.is_empty() { None } else { Some(ip.to_string()) };
    let payload = serde_json::json!({
        "level":     level,
        "message":   message,
        "context":   &ctx_str,
        "ip":        &ip_opt,
        "timestamp": crate::util::time::now().rfc3339(),
    })
    .to_string();

    let channel = channel_logs(&state.config.notify_prefix);
    tokio::join!(
        async {
            if let Err(e) = state
                .store
                .record_log(level, message, ctx_str.as_deref(), ip_opt.as_deref())
                .await
            {
                tracing::error!("[events] log insert failed: {e}");
            }
        },
        publish(state, &channel, None, &payload),
    );
}

/// Publishes a status change, with the error message and category resolved.
///
/// This is the payload `GET /swish/status/{reference}` returns, so a client that missed a live
/// notification can recover through the same handler.
pub async fn publish_swish_update(
    reference: &str,
    status:     &str,
    swish_ref:  Option<&str>,
    error_code: Option<&str>,
    state:      &SharedState,
) {
    let info = crate::domain::errors::describe_failure(status, error_code);

    let payload = serde_json::json!({
        "reference":     reference,
        "status":         status,
        "swish_ref":      swish_ref,
        "error_code":     error_code,
        "error_message":  info.map(|i| i.message(state.config.error_language)),
        "error_category": info.map(|i| i.category.as_str()),
    });
    let channel = channel_updates(&state.config.notify_prefix);
    publish(state, &channel, Some(reference), &payload.to_string()).await;
}

// Every announcement goes two ways: to in-process SSE subscribers, and out through the
// configured notifier for listeners in other processes.
async fn publish(state: &SharedState, channel: &str, reference: Option<&str>, payload: &str) {
    state.stream.send(StreamEvent {
        channel: channel.to_string(),
        reference: reference.map(str::to_string),
        payload: payload.to_string(),
    });

    if let Err(e) = state.notifier.publish(channel, payload).await {
        tracing::warn!("notify failed on '{channel}': {e}");
    }
}

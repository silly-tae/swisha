//! Telling other processes that a payout changed.
//!
//! PostgreSQL `LISTEN`/`NOTIFY`. Separate from [`events`](crate::events), which feeds
//! subscribers inside this process.

pub mod postgres;

use crate::error::Result;

/// Live notification of payout state changes.
///
/// Best effort and lossy: nothing replays a missed message. That is why
/// `GET /swish/status/{reference}` exists, and why nothing depends on delivery.
pub trait Notifier: Send + Sync {
    /// Publishes one payload on one channel. A failure is logged, never fatal: a lost
    /// notification must not fail a payout.
    fn publish(&self, channel: &str, payload: &str) -> impl Future<Output = Result<()>> + Send;
}

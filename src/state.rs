//! Shared application state, built once at startup and handed to every request.

use std::{sync::Arc, time::SystemTime};
use ring::signature::RsaKeyPair;
use tokio::sync::RwLock;
use crate::config::Config;
use crate::events::EventStream;

// Concrete types, not trait objects: no dyn dispatch, no boxed futures, and no generics
// threaded through every handler.
/// Where payouts are stored.
pub type Store = crate::store::postgres::PostgresStore;
/// How other processes are told about changes.
pub type Notifications = crate::notify::postgres::PostgresNotifier;

/// Everything a request handler needs.
pub struct AppState {
    /// Where payouts live.
    pub store:        Store,
    /// Cross-process notification.
    pub notifier:     Notifications,
    /// In-process fan-out to SSE subscribers.
    pub stream:       EventStream,
    /// The resolved configuration.
    pub config:       Config,
    /// The mTLS client that talks to Swish.
    pub swish_client: reqwest::Client,
    /// The key that signs payloads, parsed once.
    pub signing_key:  Arc<RsaKeyPair>,
    /// When the process started, for the health endpoint.
    pub started_at:   SystemTime,
    // Only the Swish reachability check is cached. The database is pinged live on every health
    // request, because a stale answer about it is the one a health endpoint must never give.
    /// The last Swish reachability check, or `None` before the first one runs.
    pub swish_probe:  RwLock<Option<SwishProbe>>,
}

/// [`AppState`] as handlers receive it.
pub type SharedState = Arc<AppState>;

/// What the last Swish reachability check found, and when.
///
/// Cached rather than run per request, because reaching Swish is slow and they rate-limit. Lives
/// here rather than with the health route so the library build, which has no HTTP layer, still
/// compiles.
#[derive(Clone, Copy)]
pub struct SwishProbe {
    /// Whether Swish answered.
    pub reachable: bool,
    /// When the check ran, so a reader can see how much to trust it.
    pub at: SystemTime,
}

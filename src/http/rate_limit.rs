//! Per-caller rate limiting on the payout endpoint.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_PER_WINDOW: u32 = 30;
const PRUNE_EVERY: u64 = 1000;

static BUCKETS: OnceLock<Mutex<HashMap<IpAddr, (u32, Instant)>>> = OnceLock::new();
static SEEN: AtomicU64 = AtomicU64::new(0);

/// How many callers are currently tracked.
///
/// The map is bounded by recent activity rather than by a fixed size, so this is the number
/// that would grow without limit if pruning ever stopped.
pub fn tracked() -> usize {
    BUCKETS
        .get()
        .map(|b| b.lock().unwrap_or_else(|e| e.into_inner()).len())
        .unwrap_or(0)
}

/// Whether this caller may make another payout request: 30 per 60 seconds.
///
/// In-process, so the limit is per instance. Running more than one instance behind a load
/// balancer multiplies the effective limit by the instance count.
pub fn allow(addr: IpAddr) -> bool {
    let buckets = BUCKETS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = buckets.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();

    // Prune periodically so the map stays bounded without a sweep on every request.
    if SEEN.fetch_add(1, Ordering::Relaxed).is_multiple_of(PRUNE_EVERY) {
        guard.retain(|_, (_, seen)| now.duration_since(*seen) < WINDOW);
    }

    let entry = guard.entry(addr).or_insert((0, now));
    if now.duration_since(entry.1) >= WINDOW {
        *entry = (1, now);
        true
    } else if entry.0 < MAX_PER_WINDOW {
        entry.0 += 1;
        true
    } else {
        false
    }
}

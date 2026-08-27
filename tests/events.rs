#![cfg(feature = "http")]

use swisha::events::{EventStream, StreamEvent, channel_events, channel_logs, channel_updates};
use swisha::http::routes::events::matches;

fn event(channel: &str, reference: Option<&str>) -> StreamEvent {
    StreamEvent {
        channel: channel.to_string(),
        reference: reference.map(str::to_string),
        payload: "{}".into(),
    }
}

#[test]
fn channels_derive_from_the_configured_prefix() {
    assert_eq!(channel_events("swisha"), "swisha:events");
    assert_eq!(channel_updates("swisha"), "swisha:updates");
    assert_eq!(channel_logs("swisha"), "swisha:logs");
    // Two services can share one database without colliding.
    assert_eq!(channel_updates("acme"), "acme:updates");
}

#[test]
fn no_filter_matches_everything() {
    assert!(matches(&event("swisha:updates", Some("INV-1")), None, None));
    assert!(matches(&event("swisha:logs", None), None, None));
}

#[test]
fn channel_filter_is_exact() {
    let e = event("swisha:updates", Some("INV-1"));
    assert!(matches(&e, Some("swisha:updates"), None));
    assert!(!matches(&e, Some("swisha:events"), None));
    assert!(!matches(&e, Some("updates"), None), "suffix alone must not match");
}

#[test]
fn reference_filter_excludes_events_without_one() {
    // A client watching one payout must not receive unrelated service logs.
    assert!(!matches(&event("swisha:logs", None), None, Some("INV-1")));
    assert!(matches(&event("swisha:updates", Some("INV-1")), None, Some("INV-1")));
    assert!(!matches(&event("swisha:updates", Some("INV-2")), None, Some("INV-1")));
}

#[test]
fn both_filters_must_hold() {
    let e = event("swisha:updates", Some("INV-1"));
    assert!(matches(&e, Some("swisha:updates"), Some("INV-1")));
    assert!(!matches(&e, Some("swisha:events"), Some("INV-1")));
    assert!(!matches(&e, Some("swisha:updates"), Some("INV-9")));
}

#[tokio::test]
async fn subscribers_receive_what_is_published() {
    let stream = EventStream::new(16);
    let mut a = stream.subscribe();
    let mut b = stream.subscribe();
    assert_eq!(stream.subscriber_count(), 2);

    // publish() is private, so drive it the way events.rs does: through a send.
    let sent = event("swisha:updates", Some("INV-1"));
    stream.publish_for_test(sent.clone());

    for rx in [&mut a, &mut b] {
        let got = rx.recv().await.unwrap();
        assert_eq!(got.channel, "swisha:updates");
        assert_eq!(got.reference.as_deref(), Some("INV-1"));
    }
}

// Publishing must never block or fail the payout path just because nobody is listening.
#[tokio::test]
async fn publishing_with_no_subscribers_is_harmless() {
    let stream = EventStream::new(4);
    assert_eq!(stream.subscriber_count(), 0);
    stream.publish_for_test(event("swisha:updates", Some("INV-1")));
}

// A subscriber that falls behind loses messages rather than stalling the producer. That is
// why GET /swish/status/:reference exists.
#[tokio::test]
async fn a_slow_subscriber_lags_instead_of_blocking() {
    let stream = EventStream::new(2);
    let mut rx = stream.subscribe();
    for n in 0..10 {
        stream.publish_for_test(event("swisha:updates", Some(&format!("INV-{n}"))));
    }
    assert!(rx.recv().await.is_err(), "an overrun subscriber reports lag");
}

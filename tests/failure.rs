#![cfg(feature = "http")]

// What swisha does when its dependencies fail underneath it. Every test here asks the same
// question: after the failure, can this produce a second payment or a lost one?
//
// The database is killed by closing its pool, which makes every subsequent query fail the way a
// dead server does. Swish is killed by pointing the client at a port nothing listens on.

mod common;

use common::{MockSwish, PHONE, SECRET, SSN};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use swisha::domain::payout::NewPayout;
use swisha::state::SharedState;
use swisha::store::PayoutStore;

static NEXT: AtomicU32 = AtomicU32::new(0);

fn caller() -> String {
    format!("198.51.100.{}", NEXT.fetch_add(1, Ordering::Relaxed) % 254 + 1)
}

struct Rig {
    internal: String,
    callback: String,
    state: SharedState,
    pool: sqlx::PgPool,
    swish: MockSwish,
}

async fn rig_with(swish: MockSwish, reachable: bool) -> Rig {
    let url = if reachable {
        swish.clone().start().await
    } else {
        // Nothing listens here, so every call is refused immediately.
        "http://127.0.0.1:1".to_string()
    };
    let mut config = common::config();
    config.swish_base_url = url;
    config.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let (state, pool) = common::state_and_pool(config).await;
    Rig {
        internal: common::serve(swisha::http::internal_router().with_state(state.clone())).await,
        callback: common::serve(swisha::http::callback_router().with_state(state.clone())).await,
        state,
        pool,
        swish,
    }
}

async fn rig() -> Rig {
    rig_with(MockSwish::new().accepts().resolves_to("PAID"), true).await
}

fn body(reference: &str) -> Value {
    json!({ "reference": reference, "payee_alias": PHONE, "payee_ssn": SSN, "amount": 100.0 })
}

async fn pay(r: &Rig, payload: Value) -> (u16, String) {
    let response = reqwest::Client::new()
        .post(format!("{}/swish/payout", r.internal))
        .header("x-api-secret", SECRET)
        .header("x-forwarded-for", caller())
        .json(&payload)
        .send()
        .await
        .expect("swisha answered");
    let code = response.status().as_u16();
    (code, response.text().await.unwrap_or_default())
}

// ---- The database dies ----

// Nothing may be sent to Swish that swisha cannot write down. A payout it cannot record is a
// payout it cannot later refuse to repeat.
#[tokio::test]
async fn a_dead_database_refuses_the_payout_before_swish_hears_about_it() {
    let r = rig().await;
    r.pool.close().await;

    let (code, _) = pay(&r, body("F-1")).await;
    assert_eq!(code, 503, "the caller is told, not left guessing");
    assert!(r.swish.posted().is_empty(), "no instruction may be sent without a record of it");
}

#[tokio::test]
async fn a_dead_database_does_not_invent_a_status() {
    let r = rig().await;
    pay(&r, body("F-2")).await;
    r.pool.close().await;

    let code = reqwest::Client::new()
        .get(format!("{}/swish/status/F-2", r.internal))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("swisha answered")
        .status()
        .as_u16();
    assert_eq!(code, 503, "an unreadable payout is not a missing one, and not a settled one");
}

// A callback that cannot be recorded must not be acknowledged as handled, or Swish stops
// redelivering it and the outcome is lost.
#[tokio::test]
async fn a_callback_that_cannot_be_recorded_is_not_acknowledged() {
    let r = rig().await;
    pay(&r, body("F-3")).await;
    r.pool.close().await;

    let code = reqwest::Client::new()
        .post(format!("{}/swish/callback", r.callback))
        .json(&json!({
            "payerPaymentReference": "F-3",
            "payoutInstructionUUID": "00000000000000000000000000000000",
            "status": "PAID",
        }))
        .send()
        .await
        .expect("swisha answered")
        .status()
        .as_u16();
    assert_eq!(code, 500, "Swish must be told to try again, not that this is done");
}

#[tokio::test]
async fn reconcile_fails_closed_when_the_store_is_unreadable() {
    use swisha::swish::reconcile::reconcile;
    let r = rig().await;
    pay(&r, body("F-4")).await;
    r.pool.close().await;

    // Must not panic, and must not decide anything it cannot verify.
    reconcile("F-4".into(), 3, r.state.clone()).await;
    assert!(r.swish.posted().len() <= 1, "reconcile never submits, least of all blind");
}

#[tokio::test]
async fn the_sweep_survives_an_unreadable_store() {
    use std::time::Duration;
    let r = rig().await;
    r.pool.close().await;
    let swept = r.state.store.claim_stalled(3, Duration::from_millis(10)).await;
    assert!(swept.is_err(), "the sweep reports the failure rather than treating it as nothing to do");
}

#[tokio::test]
async fn swisha_keeps_answering_after_a_failed_request() {
    let r = rig().await;
    r.pool.close().await;
    for i in 0..5 {
        let (code, _) = pay(&r, body(&format!("F-5-{i}"))).await;
        assert_eq!(code, 503, "request {i}");
    }
    // The process is still up and still refusing cleanly rather than having fallen over.
    let health = reqwest::Client::new()
        .get(format!("{}/system/health", r.internal))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("swisha is still serving")
        .status()
        .as_u16();
    assert_ne!(health, 0);
}

// ---- Swish fails ----

#[tokio::test]
async fn an_unreachable_swish_leaves_the_payout_marked_error() {
    let r = rig_with(MockSwish::new(), false).await;
    let (code, _) = pay(&r, body("S-1")).await;
    assert_eq!(code, 503);

    let snapshot = r.state.store.snapshot("S-1").await.expect("store").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("ERROR"));
}

// The dangerous shape: Swish took the instruction and then swisha could not confirm what became
// of it. The row says ERROR, but the money may well have moved.
#[tokio::test]
async fn an_accepted_but_unconfirmable_payout_is_never_resubmitted() {
    // Accepts the POST, then has no record of the UUID on the GET.
    let r = rig_with(MockSwish::new().accepts(), true).await;
    assert_eq!(pay(&r, body("S-2")).await.0, 202, "Swish accepted it");
    tokio::time::sleep(std::time::Duration::from_secs(17)).await;

    let snapshot = r.state.store.snapshot("S-2").await.expect("store").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("ERROR"), "swisha could not confirm it");

    // This is the case that used to resubmit. One instruction was sent; one is all there is.
    for _ in 0..3 {
        assert_eq!(pay(&r, body("S-2")).await.0, 409, "ERROR is not permission to pay again");
    }
    assert_eq!(r.swish.posted().len(), 1, "a second instruction would be a second debit");
}

#[tokio::test]
async fn a_swish_server_error_is_not_treated_as_acceptance() {
    let r = rig_with(MockSwish::new().rejects(500, "upstream exploded"), true).await;
    let (code, _) = pay(&r, body("S-3")).await;
    assert_eq!(code, 503);

    let snapshot = r.state.store.snapshot("S-3").await.expect("store").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("ERROR"));
    assert_eq!(r.swish.posted().len(), 1, "it was attempted exactly once");
}

#[tokio::test]
async fn swish_rate_limiting_us_does_not_lose_the_payout() {
    let r = rig_with(MockSwish::new().rejects(429, "Too Many Requests"), true).await;
    assert_eq!(pay(&r, body("S-4")).await.0, 503);

    let snapshot = r.state.store.snapshot("S-4").await.expect("store").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("ERROR"), "the attempt is recorded, not forgotten");
    assert_eq!(pay(&r, body("S-4")).await.0, 409, "and the reference stays spent");
}

// ---- The process dies ----

// A crash between claiming the reference and sending the instruction leaves a row whose UUID
// Swish has never seen. Recovery must not assume either way: it asks, and when Swish has no
// record it hands the payout to a person rather than paying it.
#[tokio::test]
async fn a_crash_between_claim_and_submit_never_pays_twice() {
    use swisha::swish::reconcile::reconcile;
    let r = rig_with(MockSwish::new(), true).await;

    // What the process had written the instant before it died.
    r.state
        .store
        .claim(&NewPayout {
            reference: "C-1",
            payee_alias: PHONE,
            payee_ssn: Some(SSN),
            amount: 100.0,
            message: "crash",
            swish_ref: "AAAABBBBCCCCDDDDEEEEFFFF00001111",
        })
        .await
        .expect("claim");

    // Recovery, as the sweep would run it after a restart.
    for attempts in 1..=3 {
        reconcile("C-1".into(), attempts, r.state.clone()).await;
    }

    let snapshot = r.state.store.snapshot("C-1").await.expect("store").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("NEEDS_REVIEW"), "a person decides, not swisha");
    assert!(r.swish.posted().is_empty(), "recovery must never send an instruction");
}

// The reference survives the crash, so the ERP retrying the same one is refused rather than
// producing a second payout for the same invoice.
#[tokio::test]
async fn a_reference_claimed_before_a_crash_stays_claimed_after_it() {
    let r = rig().await;
    r.state
        .store
        .claim(&NewPayout {
            reference: "C-2",
            payee_alias: PHONE,
            payee_ssn: Some(SSN),
            amount: 100.0,
            message: "crash",
            swish_ref: "AAAABBBBCCCCDDDDEEEEFFFF00002222",
        })
        .await
        .expect("claim");

    let (code, _) = pay(&r, body("C-2")).await;
    assert_eq!(code, 409, "the claim outlived the process that made it");
    assert!(r.swish.posted().is_empty());
}

// State lives in the database, so a fresh process sees exactly what the dead one left behind.
#[tokio::test]
async fn a_restarted_process_sees_the_same_payout() {
    let r = rig().await;
    pay(&r, body("C-3")).await;
    let before = r.state.store.snapshot("C-3").await.expect("store").expect("row");

    // A second router over the same store is what a restart looks like from the outside.
    let restarted = common::serve(
        swisha::http::internal_router().with_state(r.state.clone()),
    )
    .await;
    let seen: Value = reqwest::Client::new()
        .get(format!("{restarted}/swish/status/C-3"))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");

    assert_eq!(seen["swish_ref"], before.swish_ref.expect("uuid"));
}

// A health check that calls a dead database reachable is worse than not having one: a probe
// reading "ok" keeps routing traffic to an instance that cannot pay anybody. The database is
// pinged live for exactly this reason, so the answer can never be stale.
#[tokio::test]
async fn health_reports_a_dead_database_immediately() {
    let r = rig().await;

    let healthy: Value = reqwest::Client::new()
        .get(format!("{}/system/health", r.internal))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    assert_eq!(healthy["db"], true);
    assert_eq!(healthy["status"], "ok");

    r.pool.close().await;

    let dead: Value = reqwest::Client::new()
        .get(format!("{}/system/health", r.internal))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    assert_eq!(dead["db"], false, "the database is down and health must say so");
    assert_eq!(dead["status"], "degraded");
}

// The Swish half is cached, so it has to say how old it is. An unknown answer is reported as
// unknown rather than guessed at, which is what a probe sees during the first moments of a boot.
#[tokio::test]
async fn health_says_how_stale_its_swish_answer_is() {
    let r = rig().await;
    let seen: Value = reqwest::Client::new()
        .get(format!("{}/system/health", r.internal))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");

    // Nothing has probed Swish in this harness, so both fields are null rather than false.
    assert!(seen["swish_online"].is_null(), "not asked yet is not the same as unreachable");
    assert!(seen["swish_checked_seconds_ago"].is_null());
    assert_eq!(seen["status"], "ok", "an unknown Swish must not read as degraded");
}

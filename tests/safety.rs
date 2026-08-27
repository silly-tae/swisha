#![cfg(feature = "http")]

// Safety under innocent failure rather than attack: crashes, races, partial answers and events
// arriving out of order. The double-payout guardrail is the centre of it, because every other
// mistake here costs a phone call and that one costs money.

mod common;

use common::{MockSwish, PHONE, SECRET, SSN};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use swisha::state::SharedState;
use swisha::store::PayoutStore;

static NEXT_CALLER: AtomicU32 = AtomicU32::new(0);

fn caller() -> String {
    format!("198.51.100.{}", NEXT_CALLER.fetch_add(1, Ordering::Relaxed) % 254 + 1)
}

struct Rig {
    internal: String,
    callback: String,
    state: SharedState,
    swish: MockSwish,
}

async fn rig_with(swish: MockSwish) -> Rig {
    let url = swish.clone().start().await;
    let mut config = common::config();
    config.swish_base_url = url;
    config.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let state = common::state_with(config).await;
    Rig {
        internal: common::serve(swisha::http::internal_router().with_state(state.clone())).await,
        callback: common::serve(swisha::http::callback_router().with_state(state.clone())).await,
        state,
        swish,
    }
}

async fn rig() -> Rig {
    rig_with(MockSwish::new().accepts().resolves_to("PAID")).await
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
        .expect("request");
    let code = response.status().as_u16();
    (code, response.text().await.unwrap_or_default())
}

async fn callback(r: &Rig, reference: &str, uuid: &str, status: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{}/swish/callback", r.callback))
        .json(&json!({
            "payerPaymentReference": reference,
            "payoutInstructionUUID": uuid,
            "status": status,
        }))
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

async fn status_of(r: &Rig, reference: &str) -> Option<String> {
    r.state.store.snapshot(reference).await.expect("store").and_then(|s| s.status)
}

fn payload_of(sent: &Value) -> Value {
    match sent["payload"].as_str() {
        Some(raw) => serde_json::from_str(raw).expect("json"),
        None => sent["payload"].clone(),
    }
}

// ---- The double-payout guardrail ----

#[tokio::test]
async fn safety_one_reference_produces_one_instruction() {
    let r = rig().await;
    assert_eq!(pay(&r, body("G-1")).await.0, 202);
    for _ in 0..10 {
        assert_eq!(pay(&r, body("G-1")).await.0, 409);
    }
    assert_eq!(r.swish.posted().len(), 1);
}

#[tokio::test]
async fn safety_every_locked_status_refuses_a_repeat() {
    for status in ["PAID", "PENDING", "DEBITED", "NEEDS_REVIEW"] {
        let r = rig().await;
        pay(&r, body("G-2")).await;
        r.state.store.set_status_unless_terminal("G-2", status).await.expect("stage");

        let (code, _) = pay(&r, body("G-2")).await;
        assert_eq!(code, 409, "a payout in {status} must not be resubmitted");
        assert_eq!(r.swish.posted().len(), 1, "{status} allowed a second instruction");
    }
}

// A refused duplicate must leave every stored field exactly as it was, or a second request could
// rewrite the amount or the recipient of a payout already in flight.
#[tokio::test]
async fn safety_a_refused_duplicate_rewrites_nothing() {
    let r = rig().await;
    let (_, first) = pay(&r, body("G-3")).await;
    let original_uuid = serde_json::from_str::<Value>(&first).expect("json")["swish_ref"]
        .as_str()
        .expect("uuid")
        .to_string();

    let mut hostile = body("G-3");
    hostile["amount"] = json!(49_000.0);
    hostile["payee_alias"] = json!("0709999999");
    hostile["payee_ssn"] = json!("194008230049");
    assert_eq!(pay(&r, hostile).await.0, 409);

    let snapshot = r.state.store.snapshot("G-3").await.expect("store").expect("row");
    assert_eq!(snapshot.swish_ref.as_deref(), Some(original_uuid.as_str()));
    let sent = payload_of(&r.swish.posted()[0]);
    assert_eq!(sent["amount"], "100.00");
    assert_eq!(sent["payeeAlias"], "46701234567");
}

#[tokio::test]
async fn safety_a_burst_from_one_caller_yields_one_instruction() {
    let r = rig().await;
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let url = r.internal.clone();
        tasks.push(tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{url}/swish/payout"))
                .header("x-api-secret", SECRET)
                .header("x-forwarded-for", caller())
                .json(&body("G-4"))
                .send()
                .await
                .expect("request")
                .status()
                .as_u16()
        }));
    }
    let mut accepted = 0;
    for t in tasks {
        if t.await.expect("join") == 202 {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(r.swish.posted().len(), 1);
}

// Three locations sharing one database is the deployment this is aimed at. Two services claiming
// the same reference at once must still produce one payout.
#[tokio::test]
async fn safety_two_services_sharing_a_store_still_pay_once() {
    let swish = MockSwish::new().accepts().resolves_to("PAID");
    let url = swish.clone().start().await;
    let mut config = common::config();
    config.swish_base_url = url;
    config.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let state = common::state_with(config).await;

    // Two routers, one state: the same shape as two processes on one database.
    let a = common::serve(swisha::http::internal_router().with_state(state.clone())).await;
    let b = common::serve(swisha::http::internal_router().with_state(state.clone())).await;

    let mut tasks = Vec::new();
    for base in [a, b] {
        for _ in 0..6 {
            let base = base.clone();
            tasks.push(tokio::spawn(async move {
                reqwest::Client::new()
                    .post(format!("{base}/swish/payout"))
                    .header("x-api-secret", SECRET)
                    .header("x-forwarded-for", caller())
                    .json(&body("G-5"))
                    .send()
                    .await
                    .expect("request")
                    .status()
                    .as_u16()
            }));
        }
    }
    let mut accepted = 0;
    for t in tasks {
        if t.await.expect("join") == 202 {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 1, "two services must not both win the same reference");
    assert_eq!(swish.posted().len(), 1);
}

// ERROR and DECLINED are the two statuses that do not lock the row. Whatever the answer is, it
// has to be deliberate, because it decides whether a reference can produce a second instruction.
#[tokio::test]
async fn safety_a_failed_payout_does_not_silently_resubmit() {
    for status in ["ERROR", "DECLINED"] {
        let r = rig().await;
        pay(&r, body("G-6")).await;
        r.state.store.set_status_unless_terminal("G-6", status).await.expect("stage");

        let (code, _) = pay(&r, body("G-6")).await;
        assert_eq!(code, 409, "a payout in {status} must be refused");
        assert_eq!(
            r.swish.posted().len(),
            1,
            "a payout in {status} produced a second instruction"
        );
    }
}

// ---- Terminal safety ----

#[tokio::test]
async fn safety_a_settled_payout_is_never_moved() {
    for terminal in ["PAID", "DEBITED"] {
        let r = rig().await;
        pay(&r, body("T-1")).await;
        r.state.store.set_status_unless_terminal("T-1", terminal).await.expect("settle");

        for attempt in ["ERROR", "DECLINED", "PENDING", "CREATED", "NEEDS_REVIEW"] {
            r.state.store.set_status_unless_terminal("T-1", attempt).await.expect("store");
            assert_eq!(
                status_of(&r, "T-1").await.as_deref(),
                Some(terminal),
                "{attempt} moved a {terminal} payout"
            );
        }
    }
}

// NEEDS_REVIEW is deliberately not terminal: swisha has stopped chasing, but a genuine late
// answer from Swish must still be able to settle the row.
#[tokio::test]
async fn safety_a_reviewed_payout_can_still_be_settled_by_swish() {
    let r = rig().await;
    let (_, first) = pay(&r, body("T-2")).await;
    let uuid = serde_json::from_str::<Value>(&first).expect("json")["swish_ref"]
        .as_str()
        .expect("uuid")
        .to_string();
    r.state.store.set_status_unless_terminal("T-2", "NEEDS_REVIEW").await.expect("stage");

    callback(&r, "T-2", &uuid, "PAID").await;
    assert_eq!(status_of(&r, "T-2").await.as_deref(), Some("PAID"));
}

// ---- Callback safety ----

#[tokio::test]
async fn safety_a_repeated_callback_is_idempotent() {
    let r = rig().await;
    let (_, first) = pay(&r, body("K-1")).await;
    let uuid = serde_json::from_str::<Value>(&first).expect("json")["swish_ref"]
        .as_str()
        .expect("uuid")
        .to_string();

    for _ in 0..5 {
        assert_eq!(callback(&r, "K-1", &uuid, "DEBITED").await, 200);
    }
    assert_eq!(status_of(&r, "K-1").await.as_deref(), Some("DEBITED"));
    assert_eq!(r.swish.posted().len(), 1, "a callback never causes an instruction");
}

// Swish can deliver out of order. A later, earlier-stage callback must not undo a settlement.
#[tokio::test]
async fn safety_an_out_of_order_callback_cannot_rewind() {
    let r = rig().await;
    let (_, first) = pay(&r, body("K-2")).await;
    let uuid = serde_json::from_str::<Value>(&first).expect("json")["swish_ref"]
        .as_str()
        .expect("uuid")
        .to_string();

    callback(&r, "K-2", &uuid, "PAID").await;
    callback(&r, "K-2", &uuid, "PENDING").await;
    callback(&r, "K-2", &uuid, "CREATED").await;
    assert_eq!(status_of(&r, "K-2").await.as_deref(), Some("PAID"));
}

#[tokio::test]
async fn safety_a_callback_answers_ok_so_swish_stops_retrying() {
    let r = rig().await;
    // Unknown reference, mismatched UUID and an unexpected status all answer 200: anything else
    // makes Swish redeliver forever.
    assert_eq!(callback(&r, "NOPE", "00000000000000000000000000000000", "PAID").await, 200);
    pay(&r, body("K-3")).await;
    assert_eq!(callback(&r, "K-3", "00000000000000000000000000000000", "PAID").await, 200);
    assert_eq!(callback(&r, "K-3", "00000000000000000000000000000000", "WAT").await, 200);
}

// ---- Reconcile and sweep ----

#[tokio::test]
async fn safety_reconciling_twice_changes_nothing_the_second_time() {
    use swisha::swish::reconcile::reconcile;
    let r = rig_with(MockSwish::new().accepts().resolves_to("DEBITED")).await;
    let (_, first) = pay(&r, body("R-1")).await;
    let uuid = serde_json::from_str::<Value>(&first).expect("json")["swish_ref"]
        .as_str()
        .expect("uuid")
        .to_string();
    r.state.store.set_status_unless_terminal("R-1", "PENDING").await.expect("stage");

    reconcile("R-1".into(), 1, r.state.clone()).await;
    let after_first = status_of(&r, "R-1").await;
    reconcile("R-1".into(), 2, r.state.clone()).await;

    assert_eq!(status_of(&r, "R-1").await, after_first);
    assert_eq!(
        r.state.store.snapshot("R-1").await.expect("store").expect("row").swish_ref.as_deref(),
        Some(uuid.as_str()),
        "reconciling must never mint a new instruction UUID"
    );
    assert_eq!(r.swish.posted().len(), 1);
}

#[tokio::test]
async fn safety_the_sweep_leaves_settled_and_reviewed_payouts_alone() {
    use std::time::Duration;
    let r = rig().await;
    for (reference, status) in [
        ("S-1", "PAID"),
        ("S-2", "DEBITED"),
        ("S-3", "NEEDS_REVIEW"),
        ("S-4", "PENDING"),
    ] {
        pay(&r, body(reference)).await;
        r.state.store.set_status_unless_terminal(reference, status).await.expect("stage");
    }
    tokio::time::sleep(Duration::from_millis(30)).await;

    let claimed: Vec<String> = r
        .state
        .store
        .claim_stalled(3, Duration::from_millis(10))
        .await
        .expect("sweep")
        .into_iter()
        .map(|p| p.reference)
        .collect();

    for settled in ["S-1", "S-2", "S-3"] {
        assert!(!claimed.contains(&settled.to_string()), "{settled} must not be swept");
    }
    assert!(claimed.contains(&"S-4".to_string()), "a stalled payout must be swept");
}

#[tokio::test]
async fn safety_the_sweep_stops_at_the_attempt_bound() {
    use std::time::Duration;
    let r = rig().await;
    pay(&r, body("S-5")).await;
    r.state.store.set_status_unless_terminal("S-5", "PENDING").await.expect("stage");

    for expected in 1..=3 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let claimed = r.state.store.claim_stalled(3, Duration::from_millis(10)).await.expect("sweep");
        let this = claimed.iter().find(|p| p.reference == "S-5").expect("claimed");
        assert_eq!(this.attempts, expected);
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    let claimed = r.state.store.claim_stalled(3, Duration::from_millis(10)).await.expect("sweep");
    assert!(
        !claimed.iter().any(|p| p.reference == "S-5"),
        "an unresolvable payout must stop being picked up, or it is swept forever"
    );
}

// ---- Money correctness ----

#[tokio::test]
async fn safety_the_amount_reaches_swish_with_exactly_two_decimals() {
    let r = rig().await;
    for sent in [1.0f64, 100.0, 99.9, 1234.5, 49_999.99] {
        let mut b = body(&format!("M-{sent}"));
        b["amount"] = json!(sent);
        assert_eq!(pay(&r, b).await.0, 202, "{sent} should be accepted");
    }
    let rendered: Vec<String> = r
        .swish
        .posted()
        .iter()
        .map(|s| payload_of(s)["amount"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(rendered, vec!["1.00", "100.00", "99.90", "1234.50", "49999.99"]);
}

// Binary floating point cannot hold every decimal exactly. What matters is that the rounding is
// the ordinary half-away-from-zero a person expects, and never rounds a payout upward by a krona.
#[tokio::test]
async fn safety_fractional_amounts_never_round_up_by_more_than_an_ore() {
    let r = rig().await;
    for sent in [100.004f64, 100.006, 1.0 + 0.1 + 0.2] {
        let mut b = body(&format!("F-{sent}"));
        b["amount"] = json!(sent);
        pay(&r, b).await;
    }
    for rendered in r.swish.posted().iter().map(|s| payload_of(s)["amount"].clone()) {
        let text = rendered.as_str().expect("amount is a string");
        let decimals = text.split('.').nth(1).expect("two decimals");
        assert_eq!(decimals.len(), 2, "{text} must carry exactly two decimals");
    }
}

#[tokio::test]
async fn safety_the_message_is_truncated_to_what_swish_accepts() {
    let r = rig().await;
    let mut b = body("M-LONG");
    b["message"] = json!("x".repeat(200));
    assert_eq!(pay(&r, b).await.0, 202);

    let message = payload_of(&r.swish.posted()[0])["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert_eq!(message.chars().count(), 50, "Swish caps the message at 50 characters");
}

#[tokio::test]
async fn safety_the_instruction_carries_the_merchant_number_not_the_payee() {
    let r = rig().await;
    pay(&r, body("M-ALIAS")).await;
    let sent = payload_of(&r.swish.posted()[0]);
    assert_eq!(sent["payerAlias"], "1234679304", "the payer is the merchant");
    assert_eq!(sent["payeeAlias"], "46701234567", "the payee is the recipient");
    assert_eq!(sent["currency"], "SEK");
    assert_eq!(sent["payoutType"], "PAYOUT");
}

// ---- Audit trail ----

#[tokio::test]
async fn safety_every_accepted_payout_leaves_a_trail() {
    let r = rig().await;
    pay(&r, body("A-1")).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let events = r.state.store.latest_error_code("A-1").await;
    assert!(events.is_ok(), "the event table must be readable");
    let snapshot = r.state.store.snapshot("A-1").await.expect("store").expect("row");
    assert!(snapshot.swish_ref.is_some(), "the instruction UUID is recorded against the payout");
}

#[tokio::test]
async fn safety_a_rejection_records_the_code_a_person_needs() {
    let r = rig_with(MockSwish::new().rejects(422, r#"[{"errorCode":"RF07"}]"#)).await;
    pay(&r, body("A-2")).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert_eq!(status_of(&r, "A-2").await.as_deref(), Some("ERROR"));
    let code = r.state.store.latest_error_code("A-2").await.expect("store");
    assert!(code.is_some(), "the failing code must be recoverable from storage");
}

#[tokio::test]
async fn safety_the_status_endpoint_tells_the_truth_at_every_stage() {
    let r = rig().await;
    let (_, first) = pay(&r, body("A-3")).await;
    let uuid = serde_json::from_str::<Value>(&first).expect("json")["swish_ref"]
        .as_str()
        .expect("uuid")
        .to_string();

    for stage in ["PENDING", "DEBITED"] {
        r.state.store.set_status_unless_terminal("A-3", stage).await.expect("stage");
        let seen: Value = reqwest::Client::new()
            .get(format!("{}/swish/status/A-3", r.internal))
            .header("x-api-secret", SECRET)
            .send()
            .await
            .expect("request")
            .json()
            .await
            .expect("json");
        assert_eq!(seen["status"], stage);
        assert_eq!(seen["swish_ref"], uuid);
    }
}

// ---- Storage contract ----

#[tokio::test]
async fn safety_a_fresh_reference_is_unaffected_by_its_neighbours() {
    let r = rig().await;
    for i in 0..5 {
        assert_eq!(pay(&r, body(&format!("N-{i}"))).await.0, 202);
    }
    assert_eq!(r.swish.posted().len(), 5, "five references, five instructions");

    let mut uuids: Vec<String> = Vec::new();
    for i in 0..5 {
        let snapshot = r.state.store.snapshot(&format!("N-{i}")).await.expect("store").expect("row");
        uuids.push(snapshot.swish_ref.expect("uuid"));
    }
    uuids.sort();
    uuids.dedup();
    assert_eq!(uuids.len(), 5, "every payout carries its own instruction UUID");
}

#[tokio::test]
async fn safety_instruction_uuids_do_not_repeat() {
    let r = rig().await;
    let mut seen = std::collections::HashSet::new();
    for i in 0..40 {
        let (_, response) = pay(&r, body(&format!("U-{i}"))).await;
        let uuid = serde_json::from_str::<Value>(&response).expect("json")["swish_ref"]
            .as_str()
            .expect("uuid")
            .to_string();
        assert_eq!(uuid.len(), 32);
        assert!(seen.insert(uuid.clone()), "{uuid} was issued twice");
    }
}

#[tokio::test]
async fn safety_state_survives_a_restart_because_it_is_not_in_memory() {
    let r = rig().await;
    pay(&r, body("P-1")).await;
    let before = r.state.store.snapshot("P-1").await.expect("store").expect("row");

    // A second router over the same store is what a restarted process sees.
    let fresh = common::serve(swisha::http::internal_router().with_state(r.state.clone())).await;
    let seen: Value = reqwest::Client::new()
        .get(format!("{fresh}/swish/status/P-1"))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");

    assert_eq!(seen["swish_ref"], before.swish_ref.expect("uuid"));
    assert_eq!(pay(&r, body("P-1")).await.0, 409, "the guard survives a restart too");
}

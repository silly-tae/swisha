#![cfg(feature = "http")]

// Adversarial tests. Every one of these is an attempt to make swisha pay twice, pay the wrong
// person, accept an unauthenticated instruction, or leak something it should not. A test here
// passing means the attack failed.

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

struct Target {
    internal: String,
    callback: String,
    state: SharedState,
    swish: MockSwish,
}

async fn target() -> Target {
    let swish = MockSwish::new().accepts().resolves_to("PAID");
    let url = swish.clone().start().await;
    let mut config = common::config();
    config.swish_base_url = url;
    config.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let state = common::state_with(config).await;
    Target {
        internal: common::serve(swisha::http::internal_router().with_state(state.clone())).await,
        callback: common::serve(swisha::http::callback_router().with_state(state.clone())).await,
        state,
        swish,
    }
}

fn body(reference: &str) -> Value {
    json!({ "reference": reference, "payee_alias": PHONE, "payee_ssn": SSN, "amount": 100.0 })
}

async fn pay(t: &Target, secret: Option<(&str, &str)>, payload: Value) -> (u16, String) {
    let mut req = reqwest::Client::new()
        .post(format!("{}/swish/payout", t.internal))
        .header("x-forwarded-for", caller())
        .json(&payload);
    if let Some((name, value)) = secret {
        req = req.header(name, value);
    }
    let r = req.send().await.expect("request");
    (r.status().as_u16(), r.text().await.unwrap_or_default())
}

async fn authed(t: &Target, payload: Value) -> (u16, String) {
    pay(t, Some(("x-api-secret", SECRET)), payload).await
}

// ---- Authentication ----

#[tokio::test]
async fn attack_no_secret_is_refused() {
    let t = target().await;
    assert_eq!(pay(&t, None, body("A-1")).await.0, 401);
}

#[tokio::test]
async fn attack_wrong_secret_is_refused() {
    let t = target().await;
    for wrong in ["", " ", "wrong", "0000000000000000000000000000000000"] {
        assert_eq!(
            pay(&t, Some(("x-api-secret", wrong)), body("A-2")).await.0,
            401,
            "{wrong:?} must not authenticate"
        );
    }
}

// A comparison that stops at the first differing byte lets an attacker walk the secret one
// character at a time. Neither a correct prefix nor a correct-plus-extra may be accepted.
#[tokio::test]
async fn attack_partial_secret_never_authenticates() {
    let t = target().await;
    for len in 1..SECRET.len() {
        assert_eq!(
            pay(&t, Some(("x-api-secret", &SECRET[..len])), body("A-3")).await.0,
            401,
            "a {len}-character prefix must not authenticate"
        );
    }
    assert_eq!(pay(&t, Some(("x-api-secret", &format!("{SECRET}x"))), body("A-3")).await.0, 401);
}

// Surrounding whitespace is stripped from a header value by HTTP itself, so a padded secret is
// the same secret arriving, not a bypass: the exact bytes are still required. Pinned so nobody
// later reads it as a hole, and so a change to that handling is noticed.
#[tokio::test]
async fn attack_padding_around_the_secret_is_the_same_secret() {
    let t = target().await;
    for padded in [format!(" {SECRET}"), format!("{SECRET} "), format!("{SECRET}\t")] {
        assert_ne!(
            pay(&t, Some(("x-api-secret", &padded)), body(&format!("A-4-{}", padded.len()))).await.0,
            401,
            "HTTP strips the padding, so this is the correct secret"
        );
    }
    // Padding *inside* the value is part of it, and must not authenticate.
    for forged in [
        format!("{} {}", &SECRET[..4], &SECRET[5..]),
        SECRET.replace("0123", "0 23"),
    ] {
        assert_eq!(
            pay(&t, Some(("x-api-secret", &forged)), body("A-4-x")).await.0,
            401,
            "{forged:?} is a different secret"
        );
    }
}

// A control character cannot be carried in a header value at all, so a secret containing one is
// unrepresentable rather than merely rejected.
#[tokio::test]
async fn attack_a_null_byte_cannot_be_smuggled_into_the_secret() {
    let forged = format!("{}\u{0}", &SECRET[..SECRET.len() - 1]);
    assert!(
        reqwest::header::HeaderValue::from_str(&forged).is_err(),
        "a null byte is not a valid header value"
    );
}

// HTTP header names are case-insensitive, so the guard must not be bypassable by changing case
// in either direction: a different case must neither slip past nor be rejected wrongly.
#[tokio::test]
async fn attack_header_case_does_not_change_the_outcome() {
    let t = target().await;
    for name in ["X-API-SECRET", "X-Api-Secret", "x-ApI-sEcReT"] {
        assert_ne!(
            pay(&t, Some((name, SECRET)), body(&format!("A-5-{name}"))).await.0,
            401,
            "{name} is the same header and carries the right secret"
        );
    }
    assert_eq!(pay(&t, Some(("X-API-SECRET", "nope")), body("A-5-x")).await.0, 401);
}

#[tokio::test]
async fn attack_every_protected_route_demands_the_secret() {
    let t = target().await;
    let http = reqwest::Client::new();
    for path in ["/system/health", "/events", "/swish/status/A-6"] {
        let code = http
            .get(format!("{}{path}", t.internal))
            .send()
            .await
            .expect("request")
            .status()
            .as_u16();
        assert_eq!(code, 401, "{path} must not answer without the secret");
    }
}

// ---- Address spoofing and rate limiting ----

// Forwarded headers are believed only from a configured proxy. A direct caller presenting one
// must not be able to choose its own rate-limit bucket or its own audit-log identity.
#[tokio::test]
async fn attack_forwarded_header_is_ignored_from_an_untrusted_peer() {
    let swish = MockSwish::new().accepts().resolves_to("PAID");
    let url = swish.start().await;
    let mut config = common::config();
    config.swish_base_url = url;
    config.trusted_proxies = Vec::new(); // nothing is trusted
    let state = common::state_with(config).await;
    let base = common::serve(swisha::http::internal_router().with_state(state)).await;

    // 31 requests, each claiming a different origin. If the claim were believed, none would be
    // limited; because it is not, they all share the loopback bucket and the last is refused.
    let http = reqwest::Client::new();
    let mut last = 0;
    for i in 0..31 {
        last = http
            .post(format!("{base}/swish/payout"))
            .header("x-api-secret", SECRET)
            .header("x-forwarded-for", format!("203.0.113.{}", i + 1))
            .json(&body(&format!("S-1-{i}")))
            .send()
            .await
            .expect("request")
            .status()
            .as_u16();
    }
    assert_eq!(last, 429, "spoofed origins must not each get their own budget");
}

#[tokio::test]
async fn attack_private_forwarded_address_is_not_believed() {
    let t = target().await;
    let http = reqwest::Client::new();
    let mut last = 0;
    for i in 0..31 {
        last = http
            .post(format!("{}/swish/payout", t.internal))
            .header("x-api-secret", SECRET)
            .header("x-forwarded-for", format!("10.0.0.{}", i + 1))
            .json(&body(&format!("S-2-{i}")))
            .send()
            .await
            .expect("request")
            .status()
            .as_u16();
    }
    assert_eq!(last, 429, "a private range is not a routable origin and must not be trusted");
}

#[tokio::test]
async fn attack_the_rate_limit_actually_stops_a_flood() {
    let t = target().await;
    let attacker = "198.51.100.254";
    let http = reqwest::Client::new();
    let mut refused = 0;
    for i in 0..40 {
        let code = http
            .post(format!("{}/swish/payout", t.internal))
            .header("x-api-secret", SECRET)
            .header("x-forwarded-for", attacker)
            .json(&body(&format!("S-3-{i}")))
            .send()
            .await
            .expect("request")
            .status()
            .as_u16();
        if code == 429 {
            refused += 1;
        }
    }
    assert!(refused >= 9, "a flood from one origin must be cut off, refused {refused}/40");
}

// ---- SQL injection ----

// Every value is bound, never interpolated. If that were not so, this reference would drop the
// table it is being written into.
#[tokio::test]
async fn attack_sql_injection_in_the_reference_is_stored_as_text() {
    let t = target().await;
    let payload = "'; DROP TABLE swisha_payouts; --";
    let (code, _) = authed(&t, body(payload)).await;
    assert_eq!(code, 202, "it is just text");

    let snapshot = t.state.store.snapshot(payload).await.expect("store alive").expect("row");
    assert!(snapshot.swish_ref.is_some(), "the row exists, so the table does");

    // And the table still answers, which it would not if the statement had run.
    assert!(t.state.store.ping().await.is_ok());
}

#[tokio::test]
async fn attack_sql_injection_in_other_fields_changes_nothing() {
    let t = target().await;
    for (field, value) in [
        ("message", "x'); DELETE FROM swisha_payouts WHERE ('1'='1"),
        ("reference", "1' OR '1'='1"),
        ("reference", "\\'; DROP TABLE swisha_events; --"),
    ] {
        let mut b = body("S-4");
        b[field] = json!(value);
        if field == "message" {
            b["reference"] = json!(format!("S-4-{}", value.len()));
        }
        let (code, _) = authed(&t, b).await;
        assert!(code == 202 || code == 400, "unexpected {code} for {field}");
        assert!(t.state.store.ping().await.is_ok(), "the store survived {value}");
    }
}

#[tokio::test]
async fn attack_injection_through_the_status_path_is_not_a_query() {
    let t = target().await;
    for probe in ["A'--", "1%27%20OR%20%271%27%3D%271", "..%2F..%2Fetc%2Fpasswd"] {
        let code = reqwest::Client::new()
            .get(format!("{}/swish/status/{probe}", t.internal))
            .header("x-api-secret", SECRET)
            .send()
            .await
            .expect("request")
            .status()
            .as_u16();
        assert!(code == 404 || code == 400, "{probe} produced {code}");
        assert!(t.state.store.ping().await.is_ok());
    }
}

// ---- Paying twice ----

#[tokio::test]
async fn attack_the_same_reference_cannot_be_paid_twice() {
    let t = target().await;
    assert_eq!(authed(&t, body("D-1")).await.0, 202);
    for _ in 0..5 {
        assert_eq!(authed(&t, body("D-1")).await.0, 409, "one reference, one payout");
    }
    assert_eq!(t.swish.posted().len(), 1, "only one instruction may reach Swish");
}

#[tokio::test]
async fn attack_a_burst_of_identical_requests_yields_one_instruction() {
    let t = target().await;
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let url = t.internal.clone();
        tasks.push(tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{url}/swish/payout"))
                .header("x-api-secret", SECRET)
                .header("x-forwarded-for", caller())
                .json(&body("D-2"))
                .send()
                .await
                .expect("request")
                .status()
                .as_u16()
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        if task.await.expect("join") == 202 {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 1, "exactly one of twelve concurrent requests may win");
    assert_eq!(t.swish.posted().len(), 1, "and exactly one instruction may be sent");
}

// A settled payout is the one thing that must never move. Nothing a caller sends may reopen it.
#[tokio::test]
async fn attack_a_settled_payout_cannot_be_reopened() {
    let t = target().await;
    authed(&t, body("D-3")).await;
    t.state.store.set_status_unless_terminal("D-3", "PAID").await.expect("settle");

    assert_eq!(authed(&t, body("D-3")).await.0, 409);
    let snapshot = t.state.store.snapshot("D-3").await.expect("store").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("PAID"));
    assert_eq!(t.swish.posted().len(), 1, "no second instruction");
}

// The stored amount must not be rewritable by a later request claiming the same reference.
#[tokio::test]
async fn attack_a_repeat_request_cannot_raise_the_amount() {
    let t = target().await;
    authed(&t, body("D-4")).await;

    let mut bigger = body("D-4");
    bigger["amount"] = json!(49_999.0);
    assert_eq!(authed(&t, bigger).await.0, 409);

    let sent = t.swish.posted();
    assert_eq!(sent.len(), 1);
    let payload: Value = match sent[0]["payload"].as_str() {
        Some(raw) => serde_json::from_str(raw).expect("json"),
        None => sent[0]["payload"].clone(),
    };
    assert_eq!(payload["amount"], "100.00", "the original amount is what was instructed");
}

// ---- What reaches Swish ----

// The instruction is signed over exactly the bytes that are sent. A reference containing quotes
// or braces must not be able to restructure that JSON.
#[tokio::test]
async fn attack_json_breaking_reference_cannot_restructure_the_instruction() {
    let t = target().await;
    let hostile = r#"a","amount":"99999","x":"b"#;
    assert_eq!(authed(&t, body(hostile)).await.0, 202);

    let sent = t.swish.posted();
    let payload: Value = match sent[0]["payload"].as_str() {
        Some(raw) => serde_json::from_str(raw).expect("json"),
        None => sent[0]["payload"].clone(),
    };
    assert_eq!(payload["amount"], "100.00", "the amount must be the one that was validated");
    assert_eq!(payload["payerPaymentReference"], hostile, "escaped, not interpreted");
}

#[tokio::test]
async fn attack_control_characters_never_reach_swish() {
    let t = target().await;
    for (field, value) in [
        ("reference", "A\r\nHost: evil"),
        ("reference", "A\u{0}B"),
        ("message", "A\r\nX-Injected: 1"),
        ("message", "A\u{0}B"),
    ] {
        let mut b = body("W-2");
        b[field] = json!(value);
        assert_eq!(authed(&t, b).await.0, 400, "{field} {value:?} must be refused");
    }
    assert!(t.swish.posted().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn attack_the_amount_ceiling_cannot_be_stepped_over() {
    let t = target().await;
    for over in [50_000.01, 50_001.0, 1_000_000.0, f64::MAX] {
        let mut b = body("W-3");
        b["amount"] = json!(over);
        assert_eq!(authed(&t, b).await.0, 400, "{over} is above the ceiling");
    }
    assert!(t.swish.posted().is_empty());
}

#[tokio::test]
async fn attack_a_negative_amount_cannot_reverse_a_payout() {
    let t = target().await;
    for under in [-0.01, -100.0, -50_000.0, 0.0] {
        let mut b = body("W-4");
        b["amount"] = json!(under);
        assert_eq!(authed(&t, b).await.0, 400, "{under} is not a payout");
    }
    assert!(t.swish.posted().is_empty());
}

#[tokio::test]
async fn attack_an_oversized_reference_is_refused_before_storage() {
    let t = target().await;
    for size in [36, 1_000, 100_000] {
        let (code, _) = authed(&t, body(&"x".repeat(size))).await;
        assert_eq!(code, 400, "{size} characters must be refused");
    }
    assert!(t.swish.posted().is_empty());
}

// ---- Callback forgery ----

async fn forge(t: &Target, reference: &str, uuid: &str, status: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{}/swish/callback", t.callback))
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

// The callback listener is the one reachable from the internet. Knowing a reference must not be
// enough to declare a payout settled: the instruction UUID has to match too.
#[tokio::test]
async fn attack_a_callback_with_the_wrong_uuid_changes_nothing() {
    let t = target().await;
    authed(&t, body("C-1")).await;
    let before = t.state.store.snapshot("C-1").await.expect("store").expect("row");

    forge(&t, "C-1", "00000000000000000000000000000000", "PAID").await;

    let after = t.state.store.snapshot("C-1").await.expect("store").expect("row");
    assert_eq!(after.status, before.status, "a guessed UUID must not settle a payout");
}

#[tokio::test]
async fn attack_a_callback_for_an_unknown_reference_is_ignored() {
    let t = target().await;
    let code = forge(&t, "NOT-A-PAYOUT", "00000000000000000000000000000000", "PAID").await;
    assert_eq!(code, 200, "Swish is told OK so it stops retrying");
    assert!(
        t.state.store.snapshot("NOT-A-PAYOUT").await.expect("store").is_none(),
        "no row may be created by a callback"
    );
}

#[tokio::test]
async fn attack_a_callback_cannot_walk_a_paid_payout_backwards() {
    let t = target().await;
    let (_, response) = authed(&t, body("C-3")).await;
    let uuid = serde_json::from_str::<Value>(&response).expect("json")["swish_ref"]
        .as_str()
        .expect("uuid")
        .to_string();
    t.state.store.set_status_unless_terminal("C-3", "PAID").await.expect("settle");

    for status in ["ERROR", "DECLINED", "PENDING", "CREATED"] {
        forge(&t, "C-3", &uuid, status).await;
        let snapshot = t.state.store.snapshot("C-3").await.expect("store").expect("row");
        assert_eq!(snapshot.status.as_deref(), Some("PAID"), "{status} must not move a settled payout");
    }
}

#[tokio::test]
async fn attack_the_callback_listener_exposes_no_other_route() {
    let t = target().await;
    let http = reqwest::Client::new();
    for path in ["/swish/payout", "/system/health", "/events", "/swish/status/C-4", "/"] {
        let code = http
            .get(format!("{}{path}", t.callback))
            .header("x-api-secret", SECRET)
            .send()
            .await
            .expect("request")
            .status()
            .as_u16();
        assert_eq!(code, 404, "{path} must not exist on the internet-facing listener");
    }
}

// ---- Disclosure ----

#[tokio::test]
async fn attack_an_internal_error_reveals_nothing_about_itself() {
    use swisha::error::{err, ApiError};
    let leaky = ApiError::Internal(err("postgres://swisha:hunter2@10.0.0.5/swisha"));
    let shown = leaky.to_string();
    assert_eq!(shown, "Internal server error.");
    assert!(!shown.contains("hunter2") && !shown.contains("10.0.0.5"), "{shown}");
}

#[tokio::test]
async fn attack_the_status_endpoint_reveals_nothing_for_an_unknown_reference() {
    let t = target().await;
    let response = reqwest::Client::new()
        .get(format!("{}/swish/status/NOT-A-PAYOUT", t.internal))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("request");
    assert_eq!(response.status().as_u16(), 404);
    let body = response.text().await.expect("body");
    assert!(!body.contains("swisha_payouts"), "no schema in the answer: {body}");
    assert!(!body.contains("SELECT"), "no SQL in the answer: {body}");
}

#[tokio::test]
async fn attack_an_unauthenticated_caller_learns_nothing_from_the_refusal() {
    let t = target().await;
    let (code, body) = pay(&t, None, body("X-1")).await;
    assert_eq!(code, 401);
    assert!(!body.contains(SECRET), "the refusal must not echo the expected secret");
    assert!(body.len() < 200, "a refusal has nothing to explain: {body}");
}

// A rejection from Swish is surfaced so a person can act on the code, but it must carry only
// what Swish said, not swisha's own configuration.
#[tokio::test]
async fn attack_a_swish_rejection_leaks_no_local_configuration() {
    let swish = MockSwish::new().rejects(422, r#"[{"errorCode":"PA02"}]"#);
    let url = swish.start().await;
    let mut config = common::config();
    config.swish_base_url = url.clone();
    config.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let state = common::state_with(config).await;
    let base = common::serve(swisha::http::internal_router().with_state(state)).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/swish/payout"))
        .header("x-api-secret", SECRET)
        .header("x-forwarded-for", caller())
        .json(&body("X-2"))
        .send()
        .await
        .expect("request")
        .text()
        .await
        .expect("body");

    assert!(response.contains("PA02"), "the code a person needs is shown");
    assert!(!response.contains(SECRET), "but not the secret");
    assert!(!response.contains("1234679304"), "nor the merchant number");
}

// ---- Request shape ----

#[tokio::test]
async fn attack_unknown_fields_cannot_smuggle_values() {
    let t = target().await;
    for smuggled in ["swish_ref", "status", "attempts", "payer_alias", "signing_key"] {
        let mut b = body("R-1");
        b[smuggled] = json!("attacker-controlled");
        let (code, _) = authed(&t, b).await;
        assert_eq!(code, 422, "{smuggled} must be refused, not ignored");
    }
    assert!(t.swish.posted().is_empty());
}

#[tokio::test]
async fn attack_a_wrong_typed_field_is_refused() {
    let t = target().await;
    for (field, value) in [
        ("amount", json!("100.00")),
        ("reference", json!(12345)),
        ("payee_alias", json!(null)),
        ("payee_ssn", json!(196408233234i64)),
    ] {
        let mut b = body("R-2");
        b[field] = value.clone();
        let (code, _) = authed(&t, b).await;
        assert_eq!(code, 422, "{field} as {value} must be refused");
    }
}

#[tokio::test]
async fn attack_a_malformed_body_never_reaches_the_handler() {
    let t = target().await;
    for raw in ["", "{", "[]", "null", r#"{"reference":"#] {
        let code = reqwest::Client::new()
            .post(format!("{}/swish/payout", t.internal))
            .header("x-api-secret", SECRET)
            .header("content-type", "application/json")
            .body(raw)
            .send()
            .await
            .expect("request")
            .status()
            .as_u16();
        assert!(code == 400 || code == 422, "{raw:?} produced {code}");
    }
    assert!(t.swish.posted().is_empty());
}

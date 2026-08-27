#![cfg(feature = "http")]

// POST /swish/payout is the largest file in the crate and had no test of its own: routing.rs
// only ever proved it answers 401. Everything here runs against a scripted stand-in for Swish,
// so the whole accept-and-submit path is exercised without touching MSS.

mod common;

use common::{MockSwish, PHONE, SECRET, SSN};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use swisha::state::SharedState;
use swisha::store::PayoutStore;

// The rate limiter is a process-wide map keyed on caller IP, so every test in this binary would
// otherwise share one 30-request budget and later tests would start answering 429. Trusting
// loopback as a proxy lets each request present its own address, which also exercises the
// forwarded-header path rather than pretending it does not exist.
static NEXT_CALLER: AtomicU32 = AtomicU32::new(0);

// A forwarded address is only believed when it is globally routable, so a private range would
// be discarded and every request would fall back to the loopback bucket. 198.51.100.0/24 is the
// documentation range and passes that check.
fn fresh_caller() -> String {
    let n = NEXT_CALLER.fetch_add(1, Ordering::Relaxed);
    format!("198.51.100.{}", n % 254 + 1)
}

async fn app_with(mock: MockSwish) -> (String, SharedState) {
    let swish = mock.start().await;
    let mut config = common::config();
    config.swish_base_url = swish;
    config.trusted_proxies = vec!["127.0.0.1".parse().expect("loopback")];
    let state = common::state_with(config).await;
    let base = common::serve(swisha::http::internal_router().with_state(state.clone())).await;
    (base, state)
}

async fn app() -> (String, SharedState) {
    app_with(MockSwish::new().accepts().resolves_to("PAID")).await
}

async fn post(base: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("{base}/swish/payout"))
        .header("x-api-secret", SECRET)
        .header("x-forwarded-for", fresh_caller())
        .json(&body)
        .send()
        .await
        .expect("request");
    let status = response.status().as_u16();
    let text = response.text().await.expect("body");
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

// The instruction is sent as a RawValue so the signed bytes stay byte-identical, which means
// it arrives nested rather than as a string.
fn payload_of(sent: &Value) -> Value {
    match sent["payload"].as_str() {
        Some(raw) => serde_json::from_str(raw).expect("payload is JSON"),
        None => sent["payload"].clone(),
    }
}

fn valid() -> Value {
    json!({ "reference": "INV-1", "payee_alias": PHONE, "payee_ssn": SSN, "amount": 100.0 })
}

fn with(field: &str, value: Value) -> Value {
    let mut body = valid();
    body[field] = value;
    body
}

#[tokio::test]
async fn a_valid_payout_is_accepted_and_stored() {
    let (base, state) = app().await;
    let (status, body) = post(&base, valid()).await;

    assert_eq!(status, 202, "body: {body}");
    assert_eq!(body["status"], "CREATED");
    assert_eq!(body["success"], true);
    assert_eq!(
        body["swish_ref"].as_str().expect("swish_ref").len(),
        32,
        "the payout instruction UUID is 32 hex characters"
    );

    let snapshot = state.store.snapshot("INV-1").await.expect("snapshot").expect("row");
    assert_eq!(snapshot.swish_ref.as_deref(), body["swish_ref"].as_str());
}

#[tokio::test]
async fn the_reference_is_checked_before_anything_is_sent() {
    let (base, _) = app().await;
    for (label, value) in [
        ("empty", json!("")),
        ("blank", json!("   ")),
        ("36 characters", json!("x".repeat(36))),
        ("control character", json!("INV\u{0}1")),
    ] {
        let (status, body) = post(&base, with("reference", value)).await;
        assert_eq!(status, 400, "{label} should be refused, got {body}");
    }
    // Exactly at the limit is allowed, so the boundary is pinned from both sides.
    let (status, _) = post(&base, with("reference", json!("x".repeat(35)))).await;
    assert_eq!(status, 202, "35 characters is the documented maximum");
}

#[tokio::test]
async fn the_amount_is_bounded_at_both_ends() {
    let (base, _) = app().await;
    for (label, value) in [
        ("zero", json!(0.0)),
        ("below one", json!(0.99)),
        ("negative", json!(-100.0)),
        ("above the ceiling", json!(50_001.0)),
    ] {
        let (status, body) = post(&base, with("amount", value)).await;
        assert_eq!(status, 400, "{label} should be refused, got {body}");
    }
    for (label, value) in [("exactly one", json!(1.0)), ("exactly the ceiling", json!(50_000.0))] {
        let mut body = with("amount", value);
        body["reference"] = json!(format!("INV-{label}"));
        let (status, _) = post(&base, body).await;
        assert_eq!(status, 202, "{label} is inside the range");
    }
}

// JSON has no NaN, but an exponent past f64's range parses to infinity rather than failing.
// Sent as raw text because the Rust literal would not compile. The finite check is what stops
// an infinite amount reaching the amount range comparison, where it would sail past the ceiling.
#[tokio::test]
async fn a_non_finite_amount_never_reaches_swish() {
    let (base, _) = app().await;
    let raw = format!(
        r#"{{"reference":"INV-INF","payee_alias":"{PHONE}","payee_ssn":"{SSN}","amount":1e309}}"#
    );
    let code = reqwest::Client::new()
        .post(format!("{base}/swish/payout"))
        .header("x-api-secret", SECRET)
        .header("x-forwarded-for", fresh_caller())
        .header("content-type", "application/json")
        .body(raw)
        .send()
        .await
        .expect("request")
        .status()
        .as_u16();
    assert_eq!(code, 400, "an infinite amount must not be treated as a number");
}

#[tokio::test]
async fn the_payee_number_must_be_a_swedish_mobile_number() {
    let (base, _) = app().await;
    for bad in ["", "12345", "+1 555 0100", "46701234567890", "not a number"] {
        let (status, body) = post(&base, with("payee_alias", json!(bad))).await;
        assert_eq!(status, 400, "{bad:?} should be refused, got {body}");
    }
}

// A caller that supplies an identity number asked Swish to check it. Dropping it silently and
// sending the payout anyway is not that check, so a bad one is refused instead.
#[tokio::test]
async fn a_malformed_ssn_is_refused_rather_than_dropped() {
    let (base, _) = app().await;
    for (label, value) in [
        ("ten digits", json!("640823-3234")),
        ("ten digits, centenarian marker", json!("250101+1234")),
        ("failing the luhn check", json!("196408233235")),
        ("thirteen digits", json!("1964082332345")),
        ("letters", json!("19640823ABCD")),
    ] {
        let (status, body) = post(&base, with("payee_ssn", value)).await;
        assert_eq!(status, 400, "{label} should be refused, got {body}");
    }
}

#[tokio::test]
async fn an_absent_or_blank_ssn_is_simply_omitted() {
    let (base, _) = app().await;
    let mut without = valid();
    without.as_object_mut().unwrap().remove("payee_ssn");
    assert_eq!(post(&base, without).await.0, 202, "payee_ssn is optional");

    let mut blank = with("payee_ssn", json!("   "));
    blank["reference"] = json!("INV-BLANK");
    assert_eq!(post(&base, blank).await.0, 202, "blank counts as absent");
}

#[tokio::test]
async fn the_twelve_digit_ssn_reaches_swish_verbatim() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (base, _) = app_with(mock.clone()).await;
    post(&base, with("payee_ssn", json!("19640823-3234"))).await;

    let sent = mock.posted();
    assert_eq!(sent.len(), 1, "exactly one instruction should have been sent");
    let payload = payload_of(&sent[0]);
    assert_eq!(
        payload["payeeSSN"], "196408233234",
        "separators are stripped, the century is not invented"
    );
}

// Asserted on what reaches Swish rather than what is stored: the recipient reads the message
// out of their Swish app, so the wire is where it actually has to be right.
#[tokio::test]
async fn the_message_defaults_to_the_template_and_can_be_overridden() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (base, _) = app_with(mock.clone()).await;

    post(&base, valid()).await;
    let mut custom = with("message", json!("Scrap metal, week 34"));
    custom["reference"] = json!("INV-2");
    post(&base, custom).await;

    let messages: Vec<String> = mock
        .posted()
        .iter()
        .map(|sent| payload_of(sent)["message"].as_str().unwrap_or_default().to_string())
        .collect();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], "INV-1", "the default template substitutes the reference");
    assert_eq!(messages[1], "Scrap metal, week 34", "a caller may override it");
}

#[tokio::test]
async fn a_control_character_in_the_message_is_refused() {
    let (base, _) = app().await;
    let (status, _) = post(&base, with("message", json!("bad\u{0}message"))).await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn unknown_fields_are_refused_rather_than_ignored() {
    let (base, _) = app().await;
    let mut body = valid();
    body["total_summa"] = json!(1234);
    let (status, _) = post(&base, body).await;
    assert_eq!(status, 422, "an outdated caller must be told, not silently trimmed");
}

#[tokio::test]
async fn a_second_payout_for_one_reference_is_refused() {
    let (base, _) = app().await;
    assert_eq!(post(&base, valid()).await.0, 202);

    let (status, body) = post(&base, valid()).await;
    assert_eq!(status, 409, "one reference is one payout, forever");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("already"),
        "the refusal should say why: {body}"
    );
}

// Swish refusing the instruction has to surface as a failure with its code recorded, not as an
// accepted payout that quietly never happens.
#[tokio::test]
async fn a_swish_rejection_is_surfaced_and_recorded() {
    let mock = MockSwish::new()
        .rejects(422, r#"[{"errorCode":"PA06","errorMessage":"Incorrect ssn format"}]"#);
    let (base, state) = app_with(mock).await;

    let (status, body) = post(&base, valid()).await;
    assert_eq!(status, 503);
    assert!(
        body["error"].as_str().unwrap_or_default().contains("PA06"),
        "the caller should see the code Swish gave: {body}"
    );

    let snapshot = state.store.snapshot("INV-1").await.expect("snapshot").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("ERROR"));
}

#[tokio::test]
async fn the_payout_endpoint_still_demands_the_secret() {
    let (base, _) = app().await;
    let code = reqwest::Client::new()
        .post(format!("{base}/swish/payout"))
        .header("x-forwarded-for", fresh_caller())
        .json(&valid())
        .send()
        .await
        .expect("request")
        .status()
        .as_u16();
    assert_eq!(code, 401, "no secret, no payout");
}

// SWISH_REQUIRE_SSN turns identity verification from a caller's convention into a guarantee of
// the instance. Swish payouts are business to consumer, so every recipient is a private
// individual and every payout can carry a personnummer; an app that always has one can make
// sure no payout ever leaves without it, whatever the caller does.
async fn app_requiring_ssn(mock: MockSwish) -> (String, SharedState) {
    let swish = mock.start().await;
    let mut config = common::config();
    config.swish_base_url = swish;
    config.trusted_proxies = vec!["127.0.0.1".parse().expect("loopback")];
    config.require_ssn = true;
    let state = common::state_with(config).await;
    let base = common::serve(swisha::http::internal_router().with_state(state.clone())).await;
    (base, state)
}

#[tokio::test]
async fn a_payout_without_an_ssn_is_refused_when_the_instance_requires_one() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (base, _state) = app_requiring_ssn(mock.clone()).await;

    for missing in [
        json!({ "reference": "SSN-1", "payee_alias": PHONE, "amount": 100.0 }),
        json!({ "reference": "SSN-2", "payee_alias": PHONE, "amount": 100.0, "payee_ssn": null }),
        json!({ "reference": "SSN-3", "payee_alias": PHONE, "amount": 100.0, "payee_ssn": "" }),
        json!({ "reference": "SSN-4", "payee_alias": PHONE, "amount": 100.0, "payee_ssn": "   " }),
    ] {
        let (status, body) = post(&base, missing.clone()).await;
        assert_eq!(status, 400, "should refuse {missing}");
        assert!(
            body["error"].as_str().unwrap_or_default().contains("payee_ssn is required"),
            "the error must say why: {body}"
        );
    }

    // Nothing reached Swish. The guard has to stop the payout, not merely complain about it.
    assert_eq!(mock.posted().len(), 0, "a refused payout must never be submitted");
}

#[tokio::test]
async fn a_payout_with_an_ssn_still_goes_out_when_the_instance_requires_one() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (base, _state) = app_requiring_ssn(mock.clone()).await;

    let (status, _) = post(
        &base,
        json!({ "reference": "SSN-OK", "payee_alias": PHONE, "payee_ssn": SSN, "amount": 100.0 }),
    )
    .await;
    assert_eq!(status, 202);
    assert_eq!(payload_of(&mock.posted()[0])["payeeSSN"], SSN);
}

// Off is the default, and off means what it always meant: absent is simply absent.
#[tokio::test]
async fn the_default_instance_still_accepts_a_payout_without_an_ssn() {
    let mock = MockSwish::new().accepts().resolves_to("PAID");
    let (base, _state) = app_with(mock.clone()).await;

    let (status, _) = post(
        &base,
        json!({ "reference": "SSN-OFF", "payee_alias": PHONE, "amount": 100.0 }),
    )
    .await;
    assert_eq!(status, 202);
    assert!(
        payload_of(&mock.posted()[0]).get("payeeSSN").is_none(),
        "payeeSSN must be left out of the payload entirely, not sent as null"
    );
}

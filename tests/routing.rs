#![cfg(feature = "http")]

// Nothing else in the suite builds a router, so the axum 0.8 upgrade broke every route while
// the tests stayed green. This binds both listeners the way main.rs does and asks them real
// questions over real HTTP.

mod common;

use common::{serve, state, SECRET};

fn payout_body() -> serde_json::Value {
    serde_json::json!({
        "reference":   "ROUTING-TEST",
        "payee_alias": common::PHONE,
        "amount":      100.0,
    })
}

// No secret header is ever sent, so a route that exists answers 401 and stops before it
// reaches storage or Swish. That makes 401 mean "present" and 404 mean "absent".
async fn status_of(base: &str, method: reqwest::Method, path: &str) -> u16 {
    let http = reqwest::Client::new();
    let url = format!("{base}{path}");
    let request = match method {
        reqwest::Method::POST => http.post(url).json(&payout_body()),
        _ => http.get(url),
    };
    request.send().await.expect("request").status().as_u16()
}

#[tokio::test]
async fn the_callback_listener_exposes_nothing_but_the_callback() {
    let base = serve(swisha::http::callback_router().with_state(state().await)).await;

    assert_eq!(
        status_of(&base, reqwest::Method::POST, "/swish/payout").await,
        404,
        "the payout endpoint must never be reachable on the internet-facing listener"
    );
    for path in ["/system/health", "/events", "/swish/status/ROUTING-TEST"] {
        assert_eq!(
            status_of(&base, reqwest::Method::GET, path).await,
            404,
            "{path} must not be on the callback listener"
        );
    }
}

#[tokio::test]
async fn the_callback_listener_still_serves_the_callback() {
    let base = serve(swisha::http::callback_router().with_state(state().await)).await;

    // A body the callback cannot parse is rejected by the extractor, which only runs once the
    // path has matched. That proves the route is there without reaching the handler.
    let code = reqwest::Client::new()
        .post(format!("{base}/swish/callback"))
        .header("content-type", "application/json")
        .body("{ not json")
        .send()
        .await
        .expect("request")
        .status()
        .as_u16();
    assert_ne!(code, 404, "the callback route is missing from its own listener");
}

#[tokio::test]
async fn the_internal_listener_serves_every_internal_route() {
    let base = serve(swisha::http::internal_router().with_state(state().await)).await;

    assert_eq!(status_of(&base, reqwest::Method::POST, "/swish/payout").await, 401);
    for path in ["/system/health", "/events", "/swish/status/ROUTING-TEST"] {
        assert_eq!(
            status_of(&base, reqwest::Method::GET, path).await,
            401,
            "{path} should exist and demand the secret"
        );
    }
}

// The 0.7 to 0.8 upgrade changed `:reference` to `{reference}`, and the old form does not
// match. A literal path proves the placeholder is still a placeholder.
#[tokio::test]
async fn the_status_route_captures_its_path_parameter() {
    let base = serve(swisha::http::internal_router().with_state(state().await)).await;

    for reference in ["INV-1001", "abc", "a-b_c.1"] {
        assert_eq!(
            status_of(&base, reqwest::Method::GET, &format!("/swish/status/{reference}")).await,
            401,
            "/swish/status/{reference} did not match the route"
        );
    }
    assert_eq!(
        status_of(&base, reqwest::Method::GET, "/swish/status/:reference").await,
        401,
        "a literal colon should be captured as a value, not treated as syntax"
    );
}

#[tokio::test]
async fn the_internal_listener_does_not_accept_callbacks() {
    let base = serve(swisha::http::internal_router().with_state(state().await)).await;

    assert_eq!(
        status_of(&base, reqwest::Method::POST, "/swish/callback").await,
        404,
        "Swish callbacks belong on the callback listener only"
    );
}

#[tokio::test]
async fn unknown_paths_fall_back_to_swishas_own_not_found() {
    for base in [
        serve(swisha::http::internal_router().with_state(state().await)).await,
        serve(swisha::http::callback_router().with_state(state().await)).await,
    ] {
        let response = reqwest::Client::new()
            .get(format!("{base}/nope"))
            .send()
            .await
            .expect("request");
        assert_eq!(response.status().as_u16(), 404);
        assert!(
            response.text().await.expect("body").contains("Not found."),
            "the fallback should be swisha's error shape, not an empty body"
        );
    }
}

// The secret is what separates a caller from the payout endpoint on a port, so its absence and
// its presence are both worth pinning at the HTTP level rather than only in secret.rs.
#[tokio::test]
async fn the_right_secret_gets_past_the_guard() {
    let base = serve(swisha::http::internal_router().with_state(state().await)).await;
    let code = reqwest::Client::new()
        .get(format!("{base}/swish/status/NOT-A-PAYOUT"))
        .header("x-api-secret", SECRET)
        .send()
        .await
        .expect("request")
        .status()
        .as_u16();
    assert_ne!(code, 401, "the configured secret should be accepted");
}

#![cfg(feature = "http")]

use axum::{http::StatusCode, response::IntoResponse};
use swisha::error::{ApiError, err};

// These strings go to API callers, so they are part of the contract.
#[test]
fn display_strings() {
    assert_eq!(ApiError::NotFound.to_string(), "Not found.");
    assert_eq!(ApiError::Unauthorized.to_string(), "Unauthorized.");
    assert_eq!(ApiError::TooManyRequests.to_string(), "Too many requests.");
    assert_eq!(ApiError::BadRequest("Ogiltigt belopp.".into()).to_string(), "Ogiltigt belopp.");
    assert_eq!(ApiError::Conflict("Redan genomförd.".into()).to_string(), "Redan genomförd.");
    assert_eq!(
        ApiError::ServiceUnavailable("Swish API unreachable.".into()).to_string(),
        "Swish API unreachable."
    );
    assert_eq!(
        ApiError::SwishRejected { code: 422, body: r#"{"errorCode":"RF07"}"#.into() }.to_string(),
        r#"Swish rejected the request (422): {"errorCode":"RF07"}"#
    );
    // The cause is never shown to a caller.
    assert_eq!(
        ApiError::Internal(err("database exploded")).to_string(),
        "Internal server error."
    );
}

#[test]
fn empty_payload_strings_stay_empty() {
    assert_eq!(ApiError::BadRequest(String::new()).to_string(), "");
    assert_eq!(
        ApiError::SwishRejected { code: 500, body: String::new() }.to_string(),
        "Swish rejected the request (500): "
    );
}

#[test]
fn http_status_mapping() {
    let cases = [
        (ApiError::NotFound, StatusCode::NOT_FOUND),
        (ApiError::Unauthorized, StatusCode::UNAUTHORIZED),
        (ApiError::TooManyRequests, StatusCode::TOO_MANY_REQUESTS),
        (ApiError::BadRequest("x".into()), StatusCode::BAD_REQUEST),
        (ApiError::Conflict("x".into()), StatusCode::CONFLICT),
        (ApiError::ServiceUnavailable("x".into()), StatusCode::SERVICE_UNAVAILABLE),
        (ApiError::SwishRejected { code: 422, body: "x".into() }, StatusCode::SERVICE_UNAVAILABLE),
        (ApiError::Internal(err("x")), StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for (error, expected) in cases {
        assert_eq!(error.into_response().status(), expected);
    }
}

#[test]
fn is_a_std_error() {
    fn assert_error<E: std::error::Error>(_: &E) {}
    assert_error(&ApiError::NotFound);
}

// The hand-written Error impl replaced thiserror and returns a real source for Internal, which
// thiserror was not generating. Nothing checked that, so the improvement could quietly rot.
#[test]
fn only_internal_carries_a_source() {
    use std::error::Error;

    let internal = ApiError::Internal(err("database exploded"));
    let source = internal.source().expect("Internal should expose its cause");
    assert!(
        source.to_string().contains("database exploded"),
        "the chain should reach the real error: {source}"
    );

    // The rest describe themselves fully, so a chain would add nothing.
    for e in [
        ApiError::NotFound,
        ApiError::Unauthorized,
        ApiError::TooManyRequests,
        ApiError::BadRequest("x".into()),
        ApiError::Conflict("x".into()),
        ApiError::ServiceUnavailable("x".into()),
        ApiError::SwishRejected { code: 422, body: "x".into() },
    ] {
        assert!(e.source().is_none(), "{e} should not carry a source");
    }
}

// Internal deliberately says nothing about the cause to a caller, while still carrying it for
// the logs. Both halves matter: one is the contract, the other is how it gets diagnosed.
#[test]
fn internal_hides_its_cause_from_the_caller_but_not_from_the_logs() {
    use std::error::Error;

    let internal = ApiError::Internal(err("connection string with a password in it"));
    assert_eq!(internal.to_string(), "Internal server error.");
    assert!(internal.source().expect("cause").to_string().contains("password"));
}

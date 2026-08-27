//! Constant-time checking of the shared secret header.

use axum::http::HeaderMap;

/// The header a caller sends the shared secret in.
pub const HEADER: &str = "x-api-secret";

// None means no secret is configured, which Config only permits behind a Unix socket or a
// loopback port. There the listener is the boundary, enforced by the kernel rather than here.
/// Whether a request may proceed. No configured secret means no check, which is only allowed
/// on a Unix socket or loopback.
pub fn authorized(headers: &HeaderMap, expected: Option<&str>) -> bool {
    match expected {
        Some(secret) => matches(headers, secret),
        None => true,
    }
}

// XOR-fold so every byte is evaluated regardless of value.
/// Constant-time comparison of the header against the expected secret.
pub fn matches(headers: &HeaderMap, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let provided = headers.get(HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    let (a, b) = (provided.as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

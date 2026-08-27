#![cfg(feature = "http")]

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use std::net::{IpAddr, SocketAddr};
use swisha::http::client_ip::client_ip;

fn peer(addr: &str) -> SocketAddr {
    format!("{addr}:40000").parse().unwrap()
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    h
}

fn trusted() -> Vec<IpAddr> {
    vec!["127.0.0.1".parse().unwrap()]
}

// An untrusted peer cannot claim to be someone else, whatever it sends.
#[test]
fn forwarded_headers_are_ignored_from_an_untrusted_peer() {
    let h = headers(&[("x-forwarded-for", "8.8.8.8"), ("x-real-ip", "1.1.1.1")]);
    let got = client_ip(&h, peer("203.0.113.9"), &trusted());
    assert_eq!(got.to_string(), "203.0.113.9");
}

#[test]
fn a_trusted_peer_is_believed() {
    let h = headers(&[("x-real-ip", "8.8.8.8")]);
    assert_eq!(client_ip(&h, peer("127.0.0.1"), &trusted()).to_string(), "8.8.8.8");
}

#[test]
fn x_forwarded_for_wins_and_only_its_first_entry_is_used() {
    let h = headers(&[("x-forwarded-for", "8.8.8.8, 10.0.0.1, 172.16.0.9"), ("x-real-ip", "1.1.1.1")]);
    assert_eq!(client_ip(&h, peer("127.0.0.1"), &trusted()).to_string(), "8.8.8.8");
}

#[test]
fn falls_through_to_the_next_header_when_the_first_is_unusable() {
    // Private addresses and garbage are both rejected, so x-real-ip gets its turn.
    for bad in ["10.0.0.1", "not-an-ip", "", "127.0.0.1"] {
        let h = headers(&[("x-forwarded-for", bad), ("x-real-ip", "8.8.8.8")]);
        assert_eq!(
            client_ip(&h, peer("127.0.0.1"), &trusted()).to_string(),
            "8.8.8.8",
            "x-forwarded-for={bad:?}"
        );
    }
}

#[test]
fn falls_back_to_the_peer_when_nothing_usable_is_forwarded() {
    for h in [
        headers(&[]),
        headers(&[("x-real-ip", "192.168.1.1")]),
        headers(&[("x-forwarded-for", "junk"), ("x-real-ip", "junk")]),
    ] {
        assert_eq!(client_ip(&h, peer("127.0.0.1"), &trusted()).to_string(), "127.0.0.1");
    }
}

// A header value that is not a valid public IP must never reach the logs verbatim.
#[test]
fn never_returns_unvalidated_header_text() {
    let h = headers(&[("x-real-ip", "1.1.1.1'; DROP TABLE x--")]);
    assert_eq!(client_ip(&h, peer("127.0.0.1"), &trusted()).to_string(), "127.0.0.1");
}

#[test]
fn ipv6_is_handled() {
    let h = headers(&[("x-forwarded-for", "2001:db8::1")]);
    assert_eq!(client_ip(&h, peer("127.0.0.1"), &trusted()).to_string(), "2001:db8::1");
}

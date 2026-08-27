#![cfg(feature = "http")]

use std::net::IpAddr;
use swisha::http::rate_limit::allow;

// The limiter keys on IP and lives in a process-wide map, so each test uses its own address.
fn ip(last: u8) -> IpAddr {
    format!("203.0.113.{last}").parse().unwrap()
}

#[test]
fn allows_exactly_thirty_per_window() {
    let addr = ip(10);
    for n in 1..=30 {
        assert!(allow(addr), "request {n} of 30 should be allowed");
    }
    assert!(!allow(addr), "the 31st request must be refused");
    assert!(!allow(addr), "and it stays refused within the window");
}

#[test]
fn separate_addresses_have_separate_budgets() {
    let a = ip(20);
    let b = ip(21);
    for _ in 0..30 {
        assert!(allow(a));
    }
    assert!(!allow(a), "a is exhausted");
    assert!(allow(b), "b must be unaffected by a");
}

#[test]
fn ipv6_is_tracked_independently_of_ipv4() {
    let v4: IpAddr = "203.0.113.30".parse().unwrap();
    let v6: IpAddr = "2001:db8::30".parse().unwrap();
    for _ in 0..30 {
        assert!(allow(v4));
    }
    assert!(!allow(v4));
    assert!(allow(v6));
}

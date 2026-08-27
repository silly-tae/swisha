//! Working out who a request came from, behind a trusted proxy or not.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, request::Parts},
};

use crate::util::net::is_public;

// The peer address, when there is one. A Unix connection has none, so this reads the extension
// directly rather than extracting ConnectInfo, and never rejects. One handler serves both.
#[derive(Debug, Clone, Copy)]
/// The connecting peer's address, or `None` over a Unix socket, which has no peer address.
pub struct PeerAddr(
    /// The address, absent on a Unix socket.
    pub Option<SocketAddr>,
);

impl<S: Send + Sync> FromRequestParts<S> for PeerAddr {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(PeerAddr(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| *addr),
        ))
    }
}

// The originating client IP behind a reverse proxy.
//
// Forwarded headers are believed only from a configured proxy, so a direct caller cannot spoof
// its address, and the value must parse as public or it never reaches the logs or the limiter.
/// Who the request came from.
///
/// A forwarded header is believed only when the immediate peer is in `trusted_proxies` **and**
/// the forwarded value is publicly routable. Anything else falls back to the peer, so a caller
/// cannot pick its own address by setting a header.
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr, trusted_proxies: &[IpAddr]) -> IpAddr {
    let peer_ip = peer.ip();
    if !trusted_proxies.contains(&peer_ip) {
        return peer_ip;
    }

    for name in ["x-forwarded-for", "x-real-ip"] {
        if let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let first = value.split(',').next().unwrap_or("").trim();
            if let Ok(ip) = first.parse::<IpAddr>()
                && is_public(ip)
            {
                return ip;
            }
        }
    }

    peer_ip
}

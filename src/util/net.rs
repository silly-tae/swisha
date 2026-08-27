//! Deciding whether an address is publicly routable.

use std::net::IpAddr;

// Public meaning globally routable: RFC1918, loopback, link-local and unique-local are not.
/// Whether an address is publicly routable.
///
/// Private, loopback, link-local and reserved ranges are not, so a forwarded header claiming
/// one is never believed.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            !v6.is_loopback()
                && !v6.is_unspecified()
                && (s[0] & 0xffc0) != 0xfe80  // link-local fe80::/10
                && (s[0] & 0xfe00) != 0xfc00  // unique-local fc00::/7
        }
    }
}

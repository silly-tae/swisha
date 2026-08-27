//! Normalizing and checking the two identifiers a payout carries.
//!
//! Both functions are pure and clock-independent: a personnummer is 12 digits or it is not,
//! with no century inference from today's date.

// Strips non-digits and normalizes to Swedish E.164 (46XXXXXXXXX).
/// Strips everything but digits and renders a Swedish number as `46XXXXXXXXX`.
pub fn normalize_phone(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if let Some(rest) = digits.strip_prefix('0') {
        format!("46{rest}")
    } else {
        digits
    }
}

// Country code 46 followed by a 9 or 10 digit subscriber number.
/// Whether a number that has been through [`normalize_phone`] is a plausible Swedish mobile.
pub fn validate_phone(normalized: &str) -> bool {
    matches!(normalized.len(), 11 | 12)
        && normalized.starts_with("46")
        && normalized.bytes().all(|b| b.is_ascii_digit())
}

// Strips separators, nothing else. The century is part of the identity, so a caller supplies it:
// guessing it from a 10-digit number sends Swish a real personnummer belonging to someone else.
/// Strips separators from a personnummer, leaving digits only.
pub fn normalize_ssn(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_digit()).collect()
}

// Luhn check on the 10 rightmost digits (YYMMDDNNNN) of a 12-digit personnummer.
//
// The input must already be exactly 12 digits. Filtering separators here instead would accept
// a 13-character string as valid, and this is public, so a caller could then send Swish one.
/// Whether 12 digits pass the Luhn check a Swedish personnummer carries.
///
/// Expects exactly 12 digits, `YYYYMMDDNNNN`, as produced by [`normalize_ssn`]. Ten-digit forms
/// are not accepted anywhere: the century is ambiguous, and guessing it is how a payout reaches
/// the wrong person.
pub fn personnummer_luhn_valid(digits12: &str) -> bool {
    if digits12.len() != 12 || !digits12.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let d: Vec<u32> = digits12.chars().skip(2).filter_map(|c| c.to_digit(10)).collect();
    let sum: u32 = d.iter().enumerate().map(|(i, &v)| {
        let m = if i % 2 == 0 { v * 2 } else { v };
        if m > 9 { m - 9 } else { m }
    }).sum();
    sum.is_multiple_of(10)
}

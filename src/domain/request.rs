//! The payout request as a caller sends it.

use serde::Deserialize;

/// A payout request.
///
/// swisha's model is Swish's model: these five fields are all the payout instruction takes from
/// outside. Anything else a consumer keeps alongside a payout, such as an invoice or an order,
/// lives in their own storage keyed by the same reference.
///
/// Unknown fields are rejected, which is what stops an outdated caller having fields silently
/// dropped and a payout going out built from whatever happened to match.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayoutRequest {
    /// The caller's own identifier, and the idempotency key. Opaque to swisha, echoed to Swish
    /// as `payerPaymentReference` and never interpreted. At most 35 characters.
    pub reference: String,
    /// The recipient's Swish number, in any format. Normalized to `46XXXXXXXXX`.
    pub payee_alias: String,
    /// The recipient's personnummer, 12 digits. Swish verifies it against the number when
    /// present. Supplied but malformed is refused rather than dropped.
    pub payee_ssn: Option<String>,
    /// Amount in SEK. At least 1, at most `SWISH_MAX_PAYOUT`.
    pub amount: f64,
    /// What the recipient reads in their Swish app. Falls back to the `SWISH_PAYOUT_MESSAGE`
    /// template, and is truncated to Swish's 50 characters before sending.
    pub message: Option<String>,
}

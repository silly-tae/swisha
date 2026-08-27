//! Values that cross the storage boundary.
//!
//! Deliberately plain: no driver types, no row handles, nothing that assumes a particular engine
//! returned them. A [`PayoutStore`](crate::store::PayoutStore) implementation converts to and
//! from these and nothing else.

/// A payout as the caller submitted it, on its way into storage.
pub struct NewPayout<'a> {
    /// The caller's own identifier, and the idempotency key. Sent to Swish as
    /// `payerPaymentReference`.
    pub reference: &'a str,
    /// The recipient's Swish number, already normalized to `46XXXXXXXXX`.
    pub payee_alias: &'a str,
    /// The recipient's personnummer as 12 digits, when the caller supplied one. Swish checks it
    /// against the phone number.
    pub payee_ssn: Option<&'a str>,
    /// Amount in SEK.
    pub amount: f64,
    /// What the recipient reads in their Swish app, already truncated to Swish's 50 characters.
    pub message: &'a str,
    /// The `payoutInstructionUUID` swisha generated for this attempt.
    pub swish_ref: &'a str,
}

/// The state of the row after a claim attempt.
pub struct ClaimOutcome {
    /// The status the row now holds.
    pub status: String,
    /// Whichever instruction reference now owns the payout: the caller's own when the claim
    /// succeeded, an earlier one when it did not.
    pub swish_ref: Option<String>,
}

impl ClaimOutcome {
    /// Whether this caller won the claim, and may therefore submit to Swish.
    ///
    /// True only when the row now carries the reference this caller supplied. Anything else
    /// means another attempt owns the payout and this one must not send.
    pub fn claimed_by(&self, swish_ref: &str) -> bool {
        self.status == "CREATED" && self.swish_ref.as_deref() == Some(swish_ref)
    }
}

/// A payout the stall sweep claimed, with the attempt count after the claim.
///
/// The caller uses that count to decide when to stop asking Swish and hand the payout to a
/// person.
pub struct StalledPayout {
    /// The payout's reference.
    pub reference: String,
    /// How many times the sweep has now asked Swish about it.
    pub attempts: i32,
}

/// What a status lookup returns.
pub struct PayoutSnapshot {
    /// The stored status, or `None` if the row has never held one.
    pub status: Option<String>,
    /// The `payoutInstructionUUID` Swish knows the payout by.
    pub swish_ref: Option<String>,
}

/// Everything the status endpoint answers with, gathered in one round trip.
pub struct StatusView {
    /// The stored status.
    pub status: Option<String>,
    /// The `payoutInstructionUUID` Swish knows the payout by.
    pub swish_ref: Option<String>,
    /// The most recent Swish error code recorded against the payout.
    pub error_code: Option<String>,
}

/// One row of the Swish event log, which is the audit trail for a payout.
pub struct EventRecord {
    /// The payout's reference.
    pub reference: String,
    /// The `payoutInstructionUUID`, when one exists yet.
    pub swish_ref: Option<String>,
    /// What happened, such as `INITIATED`, `DEBITED` or `ERROR`.
    pub event: String,
    /// The status at the time, when the event carries one.
    pub status: Option<String>,
    /// Amount in SEK. Present on events where it is meaningful.
    pub amount: Option<f64>,
    /// The recipient's Swish number.
    pub payee_alias: Option<String>,
    /// The Swish error code, on failures.
    pub error_code: Option<String>,
    /// The error text, on failures.
    pub error_message: Option<String>,
    /// The address the request came from.
    pub ip: Option<String>,
}

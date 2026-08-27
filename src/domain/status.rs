//! The payout lifecycle, and the three sets that guard it.
//!
//! [`TERMINAL`], [`FIELDS_LOCKED`] and [`STALLED`] are the only place status membership is
//! decided. Every SQL guard is built from them through [`sql_list`], so a guard cannot drift
//! from the status it is meant to protect.

use PayoutStatus::{Created, Debited, Declined, Error, NeedsReview, Paid, Pending};

/// Where a payout is in its lifecycle.
///
/// Swish sends these uppercase, and [`parse`](PayoutStatus::parse) is case sensitive to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayoutStatus {
    /// Claimed in the database. Nothing has been sent yet, or the outcome is not known.
    Created,
    /// Swish has the instruction and has not resolved it.
    Pending,
    /// The money has left the merchant account. Terminal.
    Debited,
    /// Swish confirmed the recipient was paid. Terminal.
    Paid,
    /// Swish refused the payout. No money moved.
    Declined,
    /// Something failed. It may have been the submit, or only the status lookup, which is why
    /// this status still locks the reference.
    Error,
    /// swisha could not resolve the payout and has stopped chasing it. Not terminal: a genuine
    /// late answer from Swish still settles it. A person decides whether to pay again.
    NeedsReview,
}

impl PayoutStatus {
    /// The wire form, as Swish writes it and as the database stores it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Created => "CREATED",
            Pending => "PENDING",
            Debited => "DEBITED",
            Paid => "PAID",
            Declined => "DECLINED",
            Error => "ERROR",
            NeedsReview => "NEEDS_REVIEW",
        }
    }

    /// Parses the wire form. Case sensitive, because Swish always sends uppercase and a
    /// lowercase value means something upstream is not what it claims to be.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "CREATED" => Created,
            "PENDING" => Pending,
            "DEBITED" => Debited,
            "PAID" => Paid,
            "DECLINED" => Declined,
            "ERROR" => Error,
            "NEEDS_REVIEW" => NeedsReview,
            _ => return None,
        })
    }

    /// Whether this status is in [`TERMINAL`], and so must never be overwritten.
    pub fn is_terminal(self) -> bool {
        TERMINAL.contains(&self)
    }

}

impl std::fmt::Display for PayoutStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Never overwritten once reached.
///
/// A late, duplicated or out-of-order callback must not move a payout out of one of these.
/// [`PayoutStatus::NeedsReview`] is deliberately absent: swisha has stopped chasing
/// the payout, but a genuine late answer from Swish should still be allowed to settle it.
pub const TERMINAL: &[PayoutStatus] = &[Paid, Debited];

/// While in one of these, a repeat payout request must leave every stored field untouched.
///
/// This is the double-payout guard, and it is **every** status on purpose: one reference
/// produces one Swish instruction, forever. [`PayoutStatus::Error`] in particular does
/// not mean Swish never received the original, only that swisha stopped being able to see what
/// happened to it, so resubmitting could debit a second time.
pub const FIELDS_LOCKED: &[PayoutStatus] =
    &[Paid, Pending, Debited, NeedsReview, Created, Error, Declined];

/// Statuses that cannot move on their own: the submit or poll died, Swish never resolved the
/// payout, or a failed poll stranded it.
///
/// The stall sweep picks these up and asks Swish what really happened. It never resubmits.
pub const STALLED: &[PayoutStatus] = &[Created, Pending, Error];

/// The one forward move allowed out of a terminal status.
///
/// Swish reports a successful payout twice, as two separate callbacks seconds apart: `DEBITED`
/// when the money leaves the merchant account, then `PAID` when the recipient has it. Refusing
/// the second would leave the final state depending on whether that callback or swisha's own
/// poll landed first, and would throw away the more informative of the two answers.
///
/// Nothing else moves out of terminal, and the reverse is a regression rather than a later
/// answer, so it stays refused.
pub const TERMINAL_ADVANCE: (PayoutStatus, PayoutStatus) = (Debited, Paid);

/// The SQL condition for "this row's status may be overwritten", for a `WHERE` clause.
///
/// `new_status` names the bind parameter carrying the incoming status, `$1` on PostgreSQL. Every
/// guard is built from this and [`sql_list`], so the sets above stay the single source of truth.
pub fn writable_condition(new_status: &str) -> String {
    let (from, to) = TERMINAL_ADVANCE;
    format!(
        "(status NOT IN ({}) OR (status = '{}' AND {new_status} = '{}'))",
        sql_list(TERMINAL),
        from.as_str(),
        to.as_str(),
    )
}

/// Renders a set for a SQL `IN` clause, as `'PAID', 'DEBITED'`.
///
/// Statement text is built from this and nothing else, so the sets above are the single source
/// of truth for every guard in the codebase.
pub fn sql_list(statuses: &[PayoutStatus]) -> String {
    statuses
        .iter()
        .map(|status| format!("'{}'", status.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

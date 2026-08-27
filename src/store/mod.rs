//! Where payouts are stored, and the contract any backend has to satisfy.
//!
//! PostgreSQL is the backend swisha ships. [`PayoutStore`] is still defined by what each
//! operation must guarantee rather than by SQL, so an engine without `RETURNING` or
//! `ON CONFLICT` can satisfy it with a transaction.
//! [`conformance`] is that contract as executable checks; an adapter is supported when it
//! passes.

pub mod conformance;

pub mod postgres;

use std::time::Duration;

use crate::domain::payout::{
    ClaimOutcome, EventRecord, NewPayout, PayoutSnapshot, StalledPayout, StatusView,
};
use crate::error::Result;

/// The storage contract, specified by what each operation must guarantee rather than how.
///
/// No signature assumes UPSERT, `RETURNING` or a single round trip, so an engine without them
/// can satisfy it with a transaction. An implementation is supported when
/// [`conformance::run`] passes against it.
///
/// The methods marked **MUST** carry the payment guarantees. Getting one of them wrong is how a
/// payout gets made twice, so read those before writing an adapter.
pub trait PayoutStore: Send + Sync {
    /// Claims a reference for payout, returning the row's state after the attempt.
    ///
    /// **MUST be atomic**: two concurrent calls for one reference yield exactly one claim. This
    /// is the double-payout guard, and everything else depends on it.
    ///
    /// A row already in a [`FIELDS_LOCKED`](crate::domain::status::FIELDS_LOCKED) status keeps
    /// every stored field, so a repeat request cannot change an amount or a recipient.
    /// Otherwise the row resets to `CREATED` and the new values apply.
    fn claim(&self, new: &NewPayout<'_>) -> impl Future<Output = Result<ClaimOutcome>> + Send;

    /// Moves a payout to `status` unless it already holds a
    /// [`TERMINAL`](crate::domain::status::TERMINAL) one, and reports whether the write applied.
    ///
    /// **MUST NOT** overwrite a terminal status, with the single exception of
    /// [`TERMINAL_ADVANCE`](crate::domain::status::TERMINAL_ADVANCE), `DEBITED` to `PAID`. A
    /// late, duplicated or out-of-order callback would otherwise move a settled payout back into
    /// flight. [`writable_condition`](crate::domain::status::writable_condition) renders the
    /// rule as SQL, so an adapter does not have to restate it.
    fn set_status_unless_terminal(
        &self,
        reference: &str,
        status: &str,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// The stored payout, or `None` when the reference is unknown.
    fn snapshot(
        &self,
        reference: &str,
    ) -> impl Future<Output = Result<Option<PayoutSnapshot>>> + Send;

    /// The payout and its most recent error code together, for the status endpoint.
    ///
    /// Separate from [`snapshot`](Self::snapshot) because the callback and reconcile paths never
    /// need the code and should not pay to look it up.
    fn status_view(
        &self,
        reference: &str,
    ) -> impl Future<Output = Result<Option<StatusView>>> + Send;

    /// Claims every payout stalled longer than `older_than`, counting one attempt against each.
    ///
    /// Rows already at `max_attempts` are left alone, so an unresolvable payout stops being
    /// picked up rather than being chased forever.
    ///
    /// **MUST NOT write a status.** The caller is about to ask Swish what happened, and a status
    /// swisha has not learned yet would be a guess. Age is a [`Duration`] rather than SQL so
    /// each engine renders it its own way.
    fn claim_stalled(
        &self,
        max_attempts: i32,
        older_than: Duration,
    ) -> impl Future<Output = Result<Vec<StalledPayout>>> + Send;

    /// The most recent error code recorded against a reference, if any.
    fn latest_error_code(
        &self,
        reference: &str,
    ) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Appends one payout event to the audit trail.
    fn record_event(&self, event: &EventRecord) -> impl Future<Output = Result<()>> + Send;

    /// Appends one service log line.
    fn record_log(
        &self,
        level: &str,
        message: &str,
        context: Option<&str>,
        ip: Option<&str>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// A cheap round trip proving the store is reachable. Called on every health request.
    fn ping(&self) -> impl Future<Output = Result<()>> + Send;
}

//! Recovering payouts that stopped moving, by reading only.

use crate::events::{SwishLogFields, log_swish_event, publish_swish_update};
use crate::store::PayoutStore;
use crate::{
    state::SharedState,
    swish::submit::{poll_payout_status, update_swish_status},
};

// How many times the sweep asks Swish about one payout before handing it to a person.
/// How many times the sweep asks Swish about one payout before handing it to a person.
pub const MAX_ATTEMPTS: i32 = 3;

// Asks Swish what became of a payout that stopped moving, and records the answer. This path
// only ever reads: swisha does not resubmit under any circumstance, because a resubmission
// needs a fresh payoutInstructionUUID that Swish cannot tie to the original, so it can debit
// twice. Deciding to pay again is a person's call.
/// Asks Swish what became of a payout that stopped moving, and records the answer.
///
/// **Reads only.** swisha does not resubmit under any circumstance, because a resubmission needs
/// a fresh `payoutInstructionUUID` that Swish cannot tie to the original, so it could debit
/// twice. After [`MAX_ATTEMPTS`] without an answer the payout becomes `NEEDS_REVIEW` and swisha
/// stops. Deciding to pay again is a person's call.
pub async fn reconcile(reference: String, attempts: i32, state: SharedState) {
    let snapshot = match state.store.snapshot(&reference).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            tracing::warn!("[reconcile] payout not found: {reference}");
            return;
        }
        Err(e) => {
            tracing::warn!("[reconcile] store error fetching {reference}: {e}");
            return;
        }
    };

    // Blank counts as absent, as it does everywhere else here. Polling an empty UUID would ask
    // Swish about the collection rather than an instruction, and read the answer as a status.
    let recorded = snapshot.swish_ref.filter(|r| !r.trim().is_empty());
    let Some(swish_ref) = recorded else {
        tracing::warn!("[reconcile] {reference} has no Swish reference to ask about");
        needs_review(&reference, None, "No Swish reference was ever recorded.", &state).await;
        return;
    };

    match poll_payout_status(&swish_ref, &state).await {
        Ok(status) if status != "PENDING" => {
            settle(&reference, &swish_ref, &status, &state).await;
        }
        Ok(_) => {
            tracing::info!("[reconcile] {reference} is still pending at Swish (attempt {attempts})");
            stop_if_exhausted(&reference, &swish_ref, attempts, "Swish has not resolved this payout.", &state).await;
        }
        Err(e) => {
            tracing::warn!("[reconcile] could not reach Swish about {reference}: {e}");
            stop_if_exhausted(&reference, &swish_ref, attempts, &format!("Swish could not be reached: {e}"), &state).await;
        }
    }
}

// Swish gave a real answer, so it is written and announced. The terminal guard in the store is
// what stops a late answer walking a settled payout backwards.
async fn settle(reference: &str, swish_ref: &str, status: &str, state: &SharedState) {
    tracing::info!("[reconcile] {reference} resolved to {status}");
    let applied = update_swish_status(reference, status, state).await;
    publish_swish_update(reference, status, Some(swish_ref), None, state).await;

    log_swish_event(
        status,
        SwishLogFields {
            reference:     reference.to_string(),
            swish_ref:     Some(swish_ref.to_string()),
            status:        Some(status.to_string()),
            amount:        None,
            payee_alias:   None,
            error_code:    None,
            error_message: None,
            ip:            None,
        },
        state,
    )
    .await;

    // Mirrors the payout route, so the audit trail reads the same whichever path settled it.
    if status == "DEBITED" && applied {
        log_swish_event(
            "PAID",
            SwishLogFields {
                reference:     reference.to_string(),
                swish_ref:     Some(swish_ref.to_string()),
                status:        Some("DEBITED".into()),
                amount:        None,
                payee_alias:   None,
                error_code:    None,
                error_message: None,
                ip:            None,
            },
            state,
        )
        .await;
    }
}

async fn stop_if_exhausted(
    reference: &str,
    swish_ref: &str,
    attempts: i32,
    why: &str,
    state: &SharedState,
) {
    if attempts < MAX_ATTEMPTS {
        return;
    }
    tracing::warn!("[reconcile] {reference} unresolved after {attempts} attempts, needs review");
    needs_review(reference, Some(swish_ref), why, state).await;
}

// swisha stops chasing and says so. NEEDS_REVIEW is not terminal, so if Swish does eventually
// answer, that answer still settles the payout instead of being blocked.
async fn needs_review(reference: &str, swish_ref: Option<&str>, why: &str, state: &SharedState) {
    update_swish_status(reference, "NEEDS_REVIEW", state).await;
    publish_swish_update(reference, "NEEDS_REVIEW", swish_ref, None, state).await;

    log_swish_event(
        "NEEDS_REVIEW",
        SwishLogFields {
            reference:     reference.to_string(),
            swish_ref:     swish_ref.map(str::to_string),
            status:        Some("NEEDS_REVIEW".into()),
            amount:        None,
            payee_alias:   None,
            error_code:    None,
            error_message: Some(why.to_string()),
            ip:            None,
        },
        state,
    )
    .await;
}

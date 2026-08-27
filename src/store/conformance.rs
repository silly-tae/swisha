//! The storage contract as executable checks.

use std::time::Duration;

use crate::domain::payout::{EventRecord, NewPayout};
use crate::error::{Result, err};
use crate::store::PayoutStore;

// The contract, as executable checks. An adapter is supported when this passes, and it is
// written against the trait alone so a fork can run it against its own implementation.
//
// Keep `prefix` short and unique per run: checks append a suffix, and a reference caps at 35.
/// Runs every check against one store, returning the names of those that passed.
///
/// `prefix` namespaces the references this creates, so repeated runs stay independent. An
/// adapter is supported when this returns without error.
pub async fn run<S: PayoutStore>(store: &S, prefix: &str) -> Result<Vec<&'static str>> {
    let mut passed = Vec::new();

    store.ping().await?;
    passed.push("ping");

    claim_is_idempotent(store, prefix).await?;
    passed.push("claim_is_idempotent");

    concurrent_claims_yield_one_winner(store, prefix).await?;
    passed.push("concurrent_claims_yield_one_winner");

    terminal_status_is_never_overwritten(store, prefix).await?;
    passed.push("terminal_status_is_never_overwritten");

    debited_still_advances_to_paid(store, prefix).await?;
    passed.push("debited_still_advances_to_paid");

    sweep_selects_only_stalled_rows(store, prefix).await?;
    passed.push("sweep_selects_only_stalled_rows");

    sweep_counts_attempts_and_stops_at_the_bound(store, prefix).await?;
    passed.push("sweep_counts_attempts_and_stops_at_the_bound");

    events_round_trip(store, prefix).await?;
    passed.push("events_round_trip");

    Ok(passed)
}

fn payout<'a>(reference: &'a str, swish_ref: &'a str) -> NewPayout<'a> {
    NewPayout {
        reference,
        payee_alias: "46701234567",
        payee_ssn: None,
        amount: 100.0,
        message: "conformance",
        swish_ref,
    }
}

fn check(condition: bool, what: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(err(format!("conformance: {what}")))
    }
}

// A second claim for a reference already in flight must not take ownership.
async fn claim_is_idempotent<S: PayoutStore>(store: &S, prefix: &str) -> Result<()> {
    let reference = format!("{prefix}-idem");
    let first = store.claim(&payout(&reference, "AAAA0001")).await?;
    check(first.claimed_by("AAAA0001"), "first claim should win")?;

    let second = store.claim(&payout(&reference, "BBBB0002")).await?;
    check(!second.claimed_by("BBBB0002"), "second claim must not win")?;
    check(
        second.swish_ref.as_deref() == Some("AAAA0001"),
        "the original swish_ref must survive a second claim",
    )
}

// The double-payout guard. Two callers racing on one reference must produce one submission.
async fn concurrent_claims_yield_one_winner<S: PayoutStore>(store: &S, prefix: &str) -> Result<()> {
    let reference = format!("{prefix}-race");
    let first = payout(&reference, "CCCC0001");
    let second = payout(&reference, "DDDD0002");
    let (a, b) = tokio::join!(store.claim(&first), store.claim(&second));
    let winners = [a?.claimed_by("CCCC0001"), b?.claimed_by("DDDD0002")]
        .into_iter()
        .filter(|won| *won)
        .count();
    check(winners == 1, "exactly one concurrent claim must win")
}

async fn terminal_status_is_never_overwritten<S: PayoutStore>(store: &S, prefix: &str) -> Result<()> {
    let reference = format!("{prefix}-terminal");
    store.claim(&payout(&reference, "EEEE0001")).await?;

    check(
        store.set_status_unless_terminal(&reference, "PAID").await?,
        "a CREATED payout should accept PAID",
    )?;
    check(
        !store.set_status_unless_terminal(&reference, "ERROR").await?,
        "a PAID payout must refuse ERROR",
    )?;
    check(
        !store.set_status_unless_terminal(&reference, "DECLINED").await?,
        "a PAID payout must refuse DECLINED and report no write",
    )?;

    let snapshot = store.snapshot(&reference).await?;
    check(
        snapshot.and_then(|s| s.status).as_deref() == Some("PAID"),
        "the payout must still be PAID",
    )
}

// Swish reports a successful payout twice: DEBITED, then PAID a few seconds later, as two
// separate callbacks. Refusing the second leaves the final state depending on whether the
// callback or the poll landed first, so this one forward step has to go through.
async fn debited_still_advances_to_paid<S: PayoutStore>(store: &S, prefix: &str) -> Result<()> {
    let reference = format!("{prefix}-advance");
    store.claim(&payout(&reference, "FFFF0001")).await?;
    store.set_status_unless_terminal(&reference, "DEBITED").await?;

    check(
        store.set_status_unless_terminal(&reference, "PAID").await?,
        "a DEBITED payout must accept the PAID that follows it",
    )?;
    check(
        store.snapshot(&reference).await?.and_then(|s| s.status).as_deref() == Some("PAID"),
        "and the payout must end up PAID",
    )?;

    // The reverse is not a later answer, it is a regression, and it stays refused.
    check(
        !store.set_status_unless_terminal(&reference, "DEBITED").await?,
        "a PAID payout must never fall back to DEBITED",
    )?;
    check(
        !store.set_status_unless_terminal(&reference, "ERROR").await?,
        "and must still refuse ERROR",
    )?;
    check(
        store.snapshot(&reference).await?.and_then(|s| s.status).as_deref() == Some("PAID"),
        "the payout must still be PAID",
    )
}

async fn sweep_selects_only_stalled_rows<S: PayoutStore>(store: &S, prefix: &str) -> Result<()> {
    let stalled = format!("{prefix}-stalled");
    let settled = format!("{prefix}-settled");
    store.claim(&payout(&stalled, "HHHH0001")).await?;
    store.claim(&payout(&settled, "IIII0002")).await?;
    store.set_status_unless_terminal(&settled, "PAID").await?;

    // A real age rather than zero. Zero makes the row's timestamp and the sweep's cutoff race
    // for the same instant, which then passes or fails on clock granularity alone. Sleeping past
    // a small threshold tests the comparison meaningfully on any engine.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let swept = claimed_references(store, 3).await?;
    check(swept.contains(&stalled), "a CREATED payout must be claimed")?;
    check(!swept.contains(&settled), "a PAID payout must never be claimed")?;

    // The claim must not invent a status: only Swish knows what happened to the payout.
    let snapshot = store.snapshot(&stalled).await?;
    check(
        snapshot.and_then(|s| s.status).as_deref() == Some("CREATED"),
        "claiming a stalled payout must leave its status alone",
    )
}

// Without a bound an unresolvable payout is picked up forever, so the counter has to advance on
// every claim and the row has to drop out once it reaches the limit.
async fn sweep_counts_attempts_and_stops_at_the_bound<S: PayoutStore>(
    store: &S,
    prefix: &str,
) -> Result<()> {
    let reference = format!("{prefix}-bound");
    store.claim(&payout(&reference, "JJJJ0001")).await?;

    for expected in 1..=3 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let claimed = store.claim_stalled(3, Duration::from_millis(10)).await?;
        let attempts = claimed
            .iter()
            .find(|p| p.reference == reference)
            .map(|p| p.attempts)
            .ok_or_else(|| err("conformance: a payout below the bound was not claimed"))?;
        check(
            attempts == expected,
            "each claim must advance the attempt count by exactly one",
        )?;
    }

    tokio::time::sleep(Duration::from_millis(30)).await;
    let swept = claimed_references(store, 3).await?;
    check(
        !swept.contains(&reference),
        "a payout at the attempt bound must not be claimed again",
    )
}

async fn claimed_references<S: PayoutStore>(store: &S, max_attempts: i32) -> Result<Vec<String>> {
    Ok(store
        .claim_stalled(max_attempts, Duration::from_millis(10))
        .await?
        .into_iter()
        .map(|p| p.reference)
        .collect())
}

async fn events_round_trip<S: PayoutStore>(store: &S, prefix: &str) -> Result<()> {
    let reference = format!("{prefix}-events");
    check(
        store.latest_error_code(&reference).await?.is_none(),
        "a reference with no events must report no error code",
    )?;

    for code in ["TA01", "RF07"] {
        store
            .record_event(&EventRecord {
                reference: reference.clone(),
                swish_ref: Some("LLLL0001".into()),
                event: "CALLBACK".into(),
                status: Some("ERROR".into()),
                amount: Some(100.0),
                payee_alias: Some("46701234567".into()),
                error_code: Some(code.into()),
                error_message: Some("conformance".into()),
                ip: None,
            })
            .await?;
    }
    check(
        store.latest_error_code(&reference).await?.as_deref() == Some("RF07"),
        "the most recent error code must be returned",
    )?;

    store.record_log("info", "conformance", None, None).await
}

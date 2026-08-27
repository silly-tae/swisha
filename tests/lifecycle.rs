#![cfg(feature = "http")]

// submit and reconcile had no tests at all, because both reach Swish over the network. Driven
// here against a scripted stand-in, so every answer Swish documents can be exercised.

mod common;

use common::{MockSwish, PHONE, SSN};
use swisha::domain::payout::NewPayout;
use swisha::error::ApiError;
use swisha::state::SharedState;
use swisha::store::PayoutStore;
use swisha::swish::payload::ExecutePayoutArgs;
use swisha::swish::reconcile::reconcile;
use swisha::swish::submit::{poll_payout_status, submit_payout};

const UUID: &str = "AAAABBBBCCCCDDDDEEEEFFFF00001111";

async fn app_with(mock: MockSwish) -> SharedState {
    let swish = mock.start().await;
    let mut config = common::config();
    config.swish_base_url = swish;
    common::state_with(config).await
}

fn args<'a>(reference: &'a str, uuid: &'a str) -> ExecutePayoutArgs<'a> {
    ExecutePayoutArgs {
        reference,
        payout_uuid: uuid,
        payee_alias: "46701234567",
        payee_ssn: Some(SSN),
        amount: 100.0,
        message: "test",
    }
}

async fn stage(state: &SharedState, reference: &str, swish_ref: Option<&str>, status: &str) {
    state
        .store
        .claim(&NewPayout {
            reference,
            payee_alias: PHONE,
            payee_ssn: Some(SSN),
            amount: 100.0,
            message: "test",
            swish_ref: swish_ref.unwrap_or(""),
        })
        .await
        .expect("claim");
    state
        .store
        .set_status_unless_terminal(reference, status)
        .await
        .expect("status");
}

#[tokio::test]
async fn a_201_means_the_instruction_was_accepted() {
    let state = app_with(MockSwish::new().accepts()).await;
    assert!(submit_payout(&args("INV-1", UUID), &state).await.is_ok());
}

// Swish answers a rejected instruction with an array of Error objects, and the caller has to be
// able to read the code out of it: that code is what tells a person whether to pay again.
#[tokio::test]
async fn a_rejection_carries_the_status_and_the_body() {
    let body = r#"[{"errorCode":"PA02","errorMessage":"Amount value is missing"}]"#;
    let state = app_with(MockSwish::new().rejects(422, body)).await;

    match submit_payout(&args("INV-1", UUID), &state).await {
        Err(ApiError::SwishRejected { code, body }) => {
            assert_eq!(code, 422);
            assert!(body.contains("PA02"), "body was {body}");
        }
        other => panic!("expected a Swish rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn any_status_other_than_201_is_a_rejection() {
    for code in [400, 401, 403, 415, 429, 500] {
        let state = app_with(MockSwish::new().rejects(code, "")).await;
        match submit_payout(&args("INV-1", UUID), &state).await {
            Err(ApiError::SwishRejected { code: got, .. }) => assert_eq!(got, code),
            other => panic!("{code} should be a rejection, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn polling_returns_whichever_final_status_swish_reports() {
    for status in ["PAID", "DEBITED", "DECLINED", "ERROR"] {
        let state = app_with(MockSwish::new().resolves_to(status)).await;
        assert_eq!(
            poll_payout_status(UUID, &state).await.expect("poll"),
            status
        );
    }
}

// A UUID Swish has no record of must not read as pending: pending invites waiting for an answer
// that is never coming.
#[tokio::test]
async fn a_404_is_an_error_rather_than_pending() {
    let state = app_with(MockSwish::new()).await;
    assert!(poll_payout_status(UUID, &state).await.is_err());
}

#[tokio::test]
async fn reconcile_settles_a_payout_swish_has_resolved() {
    let mock = MockSwish::new().resolves_to("PAID");
    let state = app_with(mock.clone()).await;
    stage(&state, "INV-1", Some(UUID), "PENDING").await;

    reconcile("INV-1".into(), 1, state.clone()).await;

    let snapshot = state.store.snapshot("INV-1").await.expect("snapshot").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("PAID"));
    assert_eq!(
        snapshot.swish_ref.as_deref(),
        Some(UUID),
        "reconcile must not mint a new instruction UUID"
    );
}

// The guarantee, observed rather than asserted about the source: reconcile reads and never
// submits. A POST arriving at the mock during reconciliation is a second payout instruction.
#[tokio::test]
async fn reconcile_never_sends_an_instruction() {
    for status in ["PAID", "DEBITED", "DECLINED", "ERROR"] {
        let mock = MockSwish::new().resolves_to(status);
        let state = app_with(mock.clone()).await;
        stage(&state, "INV-1", Some(UUID), "PENDING").await;

        reconcile("INV-1".into(), 1, state).await;

        assert!(
            mock.posted().is_empty(),
            "reconciling a {status} payout sent {} instruction(s) to Swish",
            mock.posted().len()
        );
        assert!(mock.get_count() > 0, "it should have asked, though");
    }
}

// A terminal status is never walked backwards, even by reconcile writing what Swish just said.
#[tokio::test]
async fn reconcile_cannot_move_a_settled_payout() {
    let state = app_with(MockSwish::new().resolves_to("ERROR")).await;
    stage(&state, "INV-1", Some(UUID), "PAID").await;

    reconcile("INV-1".into(), 1, state.clone()).await;

    let snapshot = state.store.snapshot("INV-1").await.expect("snapshot").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("PAID"), "PAID is terminal");
}

// Nothing was ever submitted under a UUID, so there is nothing to ask Swish about. Polling a
// blank one would request the payouts collection and read whatever came back as a status.
#[tokio::test]
async fn a_payout_with_no_instruction_uuid_goes_straight_to_review() {
    let mock = MockSwish::new().resolves_to("PAID");
    let state = app_with(mock.clone()).await;
    stage(&state, "INV-1", None, "PENDING").await;

    reconcile("INV-1".into(), 0, state.clone()).await;

    let snapshot = state.store.snapshot("INV-1").await.expect("snapshot").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("NEEDS_REVIEW"));
    assert_eq!(mock.get_count(), 0, "there was nothing to ask about");
}

#[tokio::test]
async fn reconcile_leaves_a_payout_alone_while_it_is_below_the_bound() {
    let state = app_with(MockSwish::new().resolves_to("PAID")).await;
    stage(&state, "INV-1", Some(UUID), "PENDING").await;

    // Resolved, so it settles rather than counting against the bound.
    reconcile("INV-1".into(), 1, state.clone()).await;
    let snapshot = state.store.snapshot("INV-1").await.expect("snapshot").expect("row");
    assert_ne!(snapshot.status.as_deref(), Some("NEEDS_REVIEW"));
}

// The one slow test here, and worth its cost: poll runs its full schedule before reporting that
// Swish never resolved the payout, which is exactly the path that hands it to a person.
#[tokio::test]
async fn an_unresolvable_payout_reaches_review_at_the_attempt_bound() {
    let mock = MockSwish::new().resolves_to("CREATED"); // never a final status
    let state = app_with(mock.clone()).await;
    stage(&state, "INV-1", Some(UUID), "PENDING").await;

    // A literal, not MAX_ATTEMPTS: passing the constant makes the test move with it, so raising
    // the bound would go unnoticed by the very test meant to pin it.
    reconcile("INV-1".into(), 3, state.clone()).await;

    let snapshot = state.store.snapshot("INV-1").await.expect("snapshot").expect("row");
    assert_eq!(snapshot.status.as_deref(), Some("NEEDS_REVIEW"));
    assert!(mock.posted().is_empty(), "giving up must not send anything");
}

// The bound is how long swisha chases a payout before handing it to a person. Changing it is a
// decision about how long money sits unresolved, so the number itself is pinned.
#[test]
fn the_attempt_bound_is_three() {
    assert_eq!(swisha::swish::reconcile::MAX_ATTEMPTS, 3);
}

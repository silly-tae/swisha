use swisha::domain::status::{FIELDS_LOCKED, PayoutStatus, STALLED, TERMINAL, sql_list};

// These strings are the exact guard lists the SQL is built from. If any of these assertions
// changes, a payment guard changed with it.
#[test]
fn sets_render_to_the_expected_sql_lists() {
    assert_eq!(sql_list(TERMINAL), "'PAID', 'DEBITED'");
    assert_eq!(
        sql_list(FIELDS_LOCKED),
        "'PAID', 'PENDING', 'DEBITED', 'NEEDS_REVIEW', 'CREATED', 'ERROR', 'DECLINED'"
    );
    assert_eq!(sql_list(STALLED), "'CREATED', 'PENDING', 'ERROR'");
}

#[test]
fn set_membership_is_exact() {
    assert_eq!(TERMINAL.len(), 2);
    assert_eq!(FIELDS_LOCKED.len(), 7, "every status locks the reference");
    assert_eq!(STALLED.len(), 3);

    // Nothing protected from being overwritten may also be swept, or the sweep would keep
    // asking Swish about a payout that is already settled.
    for status in TERMINAL {
        assert!(!STALLED.contains(status), "{status} is both TERMINAL and STALLED");
    }
}

// swisha stops chasing a NEEDS_REVIEW payout, but it is not a settled one. All three of these
// have to hold together, and each guards something different.
#[test]
fn needs_review_stops_the_sweep_without_freezing_the_payout() {
    let status = PayoutStatus::NeedsReview;

    assert!(
        !TERMINAL.contains(&status),
        "a genuine late PAID from Swish must still be able to settle the payout"
    );
    assert!(
        !STALLED.contains(&status),
        "the sweep must not pick it up again once a person has been handed the payout"
    );
    assert!(
        FIELDS_LOCKED.contains(&status),
        "a repeat POST must still be refused and must never overwrite the stored amount"
    );
}

// The one move out of a terminal status that is allowed, and its shape in SQL. Swish sends
// DEBITED and PAID as two callbacks seconds apart; a live MSS payout confirmed both arrive.
#[test]
fn debited_is_the_only_terminal_status_that_may_advance() {
    use swisha::domain::status::{TERMINAL_ADVANCE, writable_condition};

    let (from, to) = TERMINAL_ADVANCE;
    assert_eq!(from, PayoutStatus::Debited);
    assert_eq!(to, PayoutStatus::Paid);
    assert!(TERMINAL.contains(&from) && TERMINAL.contains(&to), "both ends stay settled");

    let sql = writable_condition("$1");
    assert_eq!(
        sql,
        "(status NOT IN ('PAID', 'DEBITED') OR (status = 'DEBITED' AND $1 = 'PAID'))"
    );

    // The exception is one-way. Nothing in the condition lets PAID fall back to DEBITED.
    assert!(!sql.contains("status = 'PAID' AND"), "PAID must not be a source: {sql}");
}

#[test]
fn every_status_round_trips() {
    for status in [
        PayoutStatus::Created,
        PayoutStatus::Pending,
        PayoutStatus::Debited,
        PayoutStatus::Paid,
        PayoutStatus::Declined,
        PayoutStatus::Error,
        PayoutStatus::NeedsReview,
    ] {
        assert_eq!(PayoutStatus::parse(status.as_str()), Some(status));
        assert_eq!(status.to_string(), status.as_str());
    }
    assert_eq!(PayoutStatus::parse("NOPE"), None);
    assert_eq!(PayoutStatus::parse("FAILED_RETRY"), None, "the old name is gone");
    assert_eq!(PayoutStatus::parse("paid"), None); // case sensitive, Swish sends uppercase
}

#[test]
fn the_terminal_predicate_matches_the_set() {
    assert!(PayoutStatus::Paid.is_terminal());
    assert!(PayoutStatus::Debited.is_terminal());
    assert!(!PayoutStatus::NeedsReview.is_terminal());
    assert!(!PayoutStatus::Declined.is_terminal());
    assert!(!PayoutStatus::Pending.is_terminal());
    assert!(!PayoutStatus::Created.is_terminal());
    assert!(!PayoutStatus::Error.is_terminal());
}

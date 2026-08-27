use swisha::domain::request::PayoutRequest;

fn parse(json: &str) -> Result<PayoutRequest, serde_json::Error> {
    serde_json::from_str(json)
}

#[test]
fn accepts_the_five_field_model() {
    let req = parse(
        r#"{"reference":"INV-1","payee_alias":"0701234567","payee_ssn":"196408233234",
            "amount":100.0,"message":"Acme AB: INV-1"}"#,
    )
    .unwrap();
    assert_eq!(req.reference, "INV-1");
    assert_eq!(req.payee_alias, "0701234567");
    assert_eq!(req.payee_ssn.as_deref(), Some("196408233234"));
    assert_eq!(req.amount, 100.0);
    assert_eq!(req.message.as_deref(), Some("Acme AB: INV-1"));
}

#[test]
fn only_reference_alias_and_amount_are_required() {
    let req = parse(r#"{"reference":"INV-2","payee_alias":"0701234567","amount":50.0}"#).unwrap();
    assert!(req.payee_ssn.is_none());
    assert!(req.message.is_none());
}

// The invoicing fields swisha used to carry are gone. A caller sending the old shape must fail
// loudly, because a payout built from whichever fields happened to match could carry an amount
// the caller never sent.
#[test]
fn the_old_erp_shape_is_rejected() {
    let old = r#"{"kvittensnr":"K-1","telefonnummer":"0701234567","amount":100.0,
                  "line_items":[],"total_summa":150.0,"oresutjamning":0}"#;
    assert!(parse(old).is_err());
}

#[test]
fn each_dropped_field_is_rejected_individually() {
    for extra in [
        r#""total_summa":150.0"#,
        r#""oresutjamning":0"#,
        r#""line_items":[]"#,
        r#""utbetalningsmetod":"swish""#,
        r#""datum":"2026-08-26""#,
        r#""kvittensnr":"K-1""#,
    ] {
        let json = format!(
            r#"{{"reference":"INV-3","payee_alias":"0701234567","amount":10.0,{extra}}}"#
        );
        assert!(parse(&json).is_err(), "should reject {extra}");
    }
}

// There is exactly one amount now, so an original and a retry cannot disagree.
#[test]
fn there_is_no_second_amount_field() {
    let json = r#"{"reference":"INV-4","payee_alias":"0701234567","amount":100.0,"total_summa":100.0}"#;
    assert!(parse(json).is_err());
}

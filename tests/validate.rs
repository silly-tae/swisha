use swisha::domain::validate::{
    normalize_phone, normalize_ssn, personnummer_luhn_valid, validate_phone,
};

#[test]
fn phone_normalize_strips_leading_zero() {
    assert_eq!(normalize_phone("0701234567"), "46701234567");
}

#[test]
fn phone_normalize_keeps_46_prefix() {
    assert_eq!(normalize_phone("46701234567"), "46701234567");
}

#[test]
fn phone_normalize_discards_separators_and_symbols() {
    for written in ["070-123 45 67", "+46 70 123 45 67", "(070) 1234567", "070.123.4567"] {
        assert_eq!(normalize_phone(written), "46701234567", "input {written:?}");
    }
}

// Only a leading zero is a national prefix. Stripping any other leading digit would silently
// send the payout to a different number.
#[test]
fn phone_normalize_only_treats_a_leading_zero_as_a_prefix() {
    assert_eq!(normalize_phone("46701234567"), "46701234567");
    assert_eq!(normalize_phone("00701234567"), "460701234567");
    assert_eq!(normalize_phone(""), "");
}

#[test]
fn phone_validate_accepts_valid() {
    assert!(validate_phone("46701234567"));
    assert!(validate_phone("467012345678")); // 10-digit subscriber
}

#[test]
fn phone_validate_rejects_short() {
    assert!(!validate_phone("467012345")); // 8-digit subscriber
}

#[test]
fn phone_validate_rejects_wrong_prefix_length_and_non_digits() {
    assert!(!validate_phone(""));
    assert!(!validate_phone("47701234567"));      // not Sweden
    assert!(!validate_phone("4670123456789"));    // 11-digit subscriber
    assert!(!validate_phone("4670123456a"));      // letter
    assert!(!validate_phone("46701234 67"));      // space
    assert!(!validate_phone("+46701234567"));     // unnormalized
    assert!(!validate_phone("4670123456\u{0f}")); // control character
}

// The century is part of the identity. swisha used to infer it from a 10-digit number, which
// silently produced a real personnummer belonging to a different person.
#[test]
fn ssn_normalize_strips_separators_and_never_invents_a_century() {
    assert_eq!(normalize_ssn("19640823-3234"), "196408233234");
    assert_eq!(normalize_ssn("196408233234"), "196408233234");
    assert_eq!(normalize_ssn("19640823 3234"), "196408233234");

    // Ten digits stay ten digits, so the caller is told rather than guessed at.
    assert_eq!(normalize_ssn("640823-3234"), "6408233234");
    assert_eq!(normalize_ssn("250101+1234"), "2501011234");
    assert_eq!(normalize_ssn(""), "");
}

#[test]
fn luhn_valid() {
    assert!(personnummer_luhn_valid("196408233234"));
}

#[test]
fn luhn_invalid() {
    assert!(!personnummer_luhn_valid("196408233235"));
}

// The doubling step folds a two-digit product with `m > 9`. A nine sitting at an undoubled
// position is the only case where `>` and `>=` disagree, so it needs its own fixture: none of
// the other numbers here would notice the difference.
#[test]
fn luhn_folds_only_products_above_nine() {
    for valid in ["194008230049", "194008230189", "194008230239"] {
        assert!(
            personnummer_luhn_valid(valid),
            "{valid} carries a 9 in an undoubled position and must stay valid"
        );
    }
}

#[test]
fn luhn_requires_exactly_twelve_digits() {
    assert!(!personnummer_luhn_valid("6408233234"), "ten digits is not enough");
    assert!(!personnummer_luhn_valid("1964082332345"), "thirteen is too many");
    assert!(!personnummer_luhn_valid(""));
}

// Non-digits are filtered rather than rejected, so a separator that survived normalization
// would shorten the sequence instead of poisoning it.
#[test]
fn luhn_rejects_anything_that_is_not_twelve_digits_after_filtering() {
    assert!(!personnummer_luhn_valid("19640823-3234"));
    assert!(!personnummer_luhn_valid("1964082332ab"));
}

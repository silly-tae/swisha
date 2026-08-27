use swisha::swish::random_payout_uuid;
use swisha::util::hex;

// Swish expects payoutInstructionUUID as 32 uppercase hex characters with no separators.
// This is also the idempotency key for the whole payout, so the shape is load-bearing.
#[test]
fn payout_uuid_shape() {
    for _ in 0..1000 {
        let id = random_payout_uuid();
        assert_eq!(id.len(), 32, "{id}");
        assert!(id.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)), "{id}");
    }
}

#[test]
fn payout_uuid_carries_v4_version_and_variant_bits() {
    for _ in 0..1000 {
        let id = random_payout_uuid();
        assert_eq!(&id[12..13], "4", "version nibble in {id}");
        assert!(matches!(&id[16..17], "8" | "9" | "A" | "B"), "variant nibble in {id}");
    }
}

#[test]
fn payout_uuids_do_not_repeat() {
    let ids: std::collections::HashSet<String> = (0..10_000).map(|_| random_payout_uuid()).collect();
    assert_eq!(ids.len(), 10_000);
}

// The shared helper must render exactly what the two hand-rolled versions did: two uppercase
// digits per byte, leading zeros preserved.
#[test]
fn hex_upper_matches_the_per_byte_format() {
    assert_eq!(hex::upper(&[]), "");
    assert_eq!(hex::upper(&[0x00, 0x0f, 0xff, 0xa5]), "000FFFA5");
    assert_eq!(hex::upper(&[0x7b, 0x2c, 0x01]), "7B2C01");
    let all: Vec<u8> = (0..=255u8).collect();
    let expected: String = all.iter().map(|b| format!("{b:02X}")).collect();
    assert_eq!(hex::upper(&all), expected);
}

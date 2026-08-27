use swisha::swish::cert::read_cert_serial;

const CERT: &str = include_str!("fixtures/cert.pem");

#[test]
fn reads_serial_as_uppercase_hex_with_der_leading_zero() {
    assert_eq!(read_cert_serial(CERT).unwrap(), "00A5FF0F2C7B01");
}

#[test]
fn rejects_input_that_is_not_a_certificate() {
    assert!(read_cert_serial("").is_err());
    assert!(read_cert_serial("-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----").is_err());
    assert!(read_cert_serial("-----BEGIN CERTIFICATE-----\nAAAA\n").is_err());
}

// Runs against Swish's real test bundle when SWISH_TEST_CERT_DIR points at it. Those files
// are 3-certificate chains, so this also proves the leaf is what gets read.
//
//   SWISH_TEST_CERT_DIR=~/Downloads/client_cert cargo test --test cert
#[test]
fn reads_the_leaf_serial_from_swish_chain_certificates() {
    let Ok(dir) = std::env::var("SWISH_TEST_CERT_DIR") else {
        eprintln!("skipped: SWISH_TEST_CERT_DIR is not set");
        return;
    };

    let cases = [
        (
            "Swish_Merchant_TestCertificate_1234679304.pem",
            "743A083EEEE494161C530EE28EF4208A",
        ),
        (
            "Swish_Merchant_TestSigningCertificate_1234679304.pem",
            "4F24C03A0295A0B53596240EA8C0F430",
        ),
    ];

    for (file, expected) in cases {
        let pem = std::fs::read_to_string(format!("{dir}/{file}")).expect("read certificate");
        assert!(
            pem.matches("BEGIN CERTIFICATE").count() > 1,
            "{file} should be a chain, so this test proves the leaf is chosen"
        );
        assert_eq!(read_cert_serial(&pem).unwrap(), expected, "{file}");
    }
}

// The mTLS and signing certificates are different, so signing with the wrong one is a real
// mistake to make. Their serials must never be confused.
#[test]
fn the_mtls_and_signing_serials_differ() {
    let Ok(dir) = std::env::var("SWISH_TEST_CERT_DIR") else {
        return;
    };
    let read = |f: &str| {
        read_cert_serial(&std::fs::read_to_string(format!("{dir}/{f}")).unwrap()).unwrap()
    };
    assert_ne!(
        read("Swish_Merchant_TestCertificate_1234679304.pem"),
        read("Swish_Merchant_TestSigningCertificate_1234679304.pem"),
    );
}

// A certificate and key that are not a pair start the service happily and are then rejected by
// Swish on every payout as PA01, "Parameter is not correct", which names no key. Four files with
// near-identical names make the mixup easy, so the pairing is checked at startup.

fn self_signed(dir: &std::path::Path, name: &str) -> (String, String) {
    let key = dir.join(format!("{name}.key"));
    let crt = dir.join(format!("{name}.crt"));
    let out = std::process::Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=test"])
        .arg("-keyout").arg(&key)
        .arg("-out").arg(&crt)
        .output()
        .expect("openssl is required to generate a throwaway pair");
    assert!(out.status.success(), "openssl req failed");
    (
        std::fs::read_to_string(&crt).expect("read certificate"),
        std::fs::read_to_string(&key).expect("read key"),
    )
}

#[test]
fn a_certificate_matches_only_its_own_key() {
    use swisha::swish::cert::certificate_matches_key;

    let dir = std::env::temp_dir().join(format!("swisha-pair-{}", swisha::swish::random_payout_uuid()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (cert_a, key_a) = self_signed(&dir, "a");
    let (cert_b, key_b) = self_signed(&dir, "b");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(certificate_matches_key(&cert_a, &key_a).expect("check"), "a pair must match itself");
    assert!(certificate_matches_key(&cert_b, &key_b).expect("check"));
    assert!(!certificate_matches_key(&cert_a, &key_b).expect("check"), "a foreign key must not match");
    assert!(!certificate_matches_key(&cert_b, &key_a).expect("check"));
}

#[test]
fn the_pair_check_rejects_input_that_is_not_a_certificate_or_key() {
    use swisha::swish::cert::certificate_matches_key;
    let dir = std::env::temp_dir().join(format!("swisha-pair2-{}", swisha::swish::random_payout_uuid()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (cert, key) = self_signed(&dir, "c");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(certificate_matches_key("not a certificate", &key).is_err());
    assert!(certificate_matches_key(&cert, "not a key").is_err());
}

// Against Swish's real test bundle: the signing pair matches, and the merchant key, which is the
// file most likely to be reached for by mistake, does not.
#[test]
fn the_swish_bundle_signing_pair_matches_and_the_merchant_key_does_not() {
    use swisha::swish::cert::certificate_matches_key;
    let Ok(dir) = std::env::var("SWISH_TEST_CERT_DIR") else {
        eprintln!("skipped: SWISH_TEST_CERT_DIR is not set");
        return;
    };
    let read = |f: &str| std::fs::read_to_string(format!("{dir}/{f}")).expect("read");
    let signing_cert = read("Swish_Merchant_TestSigningCertificate_1234679304.pem");
    let signing_key = read("Swish_Merchant_TestSigningCertificate_1234679304.key");
    let merchant_key = read("Swish_Merchant_TestCertificate_1234679304.key");

    assert!(certificate_matches_key(&signing_cert, &signing_key).expect("check"));
    assert!(!certificate_matches_key(&signing_cert, &merchant_key).expect("check"));
}

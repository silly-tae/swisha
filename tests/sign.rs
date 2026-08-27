// Swish hashes the payload twice: swisha pre-hashes with SHA-512 and ring hashes again before
// signing. Getting that wrong produces a signature Swish rejects, and nothing else notices.

use ring::signature::{RsaKeyPair, UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA512};
use swisha::swish::sign::{pem_to_der, sign_payload};
use swisha::util::base64;

fn key() -> RsaKeyPair {
    let out = std::process::Command::new("openssl")
        .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048"])
        .output()
        .expect("openssl is required to generate a throwaway signing key");
    assert!(out.status.success(), "openssl genpkey failed");
    let pem = String::from_utf8(out.stdout).expect("PEM is UTF-8");
    RsaKeyPair::from_pkcs8(&pem_to_der(&pem).expect("decode PEM")).expect("parse key")
}

// The signature is verified against SHA512(payload), because ring applies the second hash. That
// is the whole double-hash contract, checked rather than described.
#[test]
fn the_signature_verifies_over_the_pre_hashed_payload() {
    let pair = key();
    let payload = r#"{"amount":"100.00","currency":"SEK"}"#;
    let signature = base64::decode(&sign_payload(payload, &pair).expect("sign")).expect("base64");

    let pre_hash = ring::digest::digest(&ring::digest::SHA512, payload.as_bytes());
    let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA512, pair.public().as_ref());
    public
        .verify(pre_hash.as_ref(), &signature)
        .expect("the signature must verify over the pre-hashed payload");
}

#[test]
fn a_signature_does_not_verify_over_the_raw_payload() {
    let pair = key();
    let payload = "hello";
    let signature = base64::decode(&sign_payload(payload, &pair).expect("sign")).expect("base64");

    let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA512, pair.public().as_ref());
    assert!(
        public.verify(payload.as_bytes(), &signature).is_err(),
        "signing the raw payload once would be the wrong contract"
    );
}

#[test]
fn a_changed_payload_invalidates_the_signature() {
    let pair = key();
    let signature = base64::decode(&sign_payload("original", &pair).expect("sign")).expect("base64");

    let tampered = ring::digest::digest(&ring::digest::SHA512, b"tampered");
    let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA512, pair.public().as_ref());
    assert!(public.verify(tampered.as_ref(), &signature).is_err());
}

#[test]
fn the_signature_is_base64_of_the_full_modulus() {
    let pair = key();
    let encoded = sign_payload("payload", &pair).expect("sign");
    let raw = base64::decode(&encoded).expect("the signature is base64");
    assert_eq!(raw.len(), pair.public().modulus_len(), "2048-bit key, 256-byte signature");
    assert!(!encoded.contains(char::is_whitespace), "no line wrapping");
}

#[test]
fn pem_decoding_ignores_the_armour_lines_and_rejects_rubbish() {
    let pair_pem = "-----BEGIN PRIVATE KEY-----\nAAECAwQ=\n-----END PRIVATE KEY-----\n";
    assert_eq!(pem_to_der(pair_pem).expect("decode"), vec![0, 1, 2, 3, 4]);
    assert!(pem_to_der("-----BEGIN PRIVATE KEY-----\nnot base64!\n-----END PRIVATE KEY-----").is_err());
}

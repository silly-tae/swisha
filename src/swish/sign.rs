//! Double SHA-512 RSA signing, as the Swish CPC reference requires.

use crate::error::{Result, err};
use ring::{digest::{digest, SHA512}, rand::SystemRandom, signature::{RsaKeyPair, RSA_PKCS1_SHA512}};

use crate::util::base64;

/// Signs a payout payload the way the CPC reference requires.
///
/// Swish hashes the payload **twice**. This pre-hashes with SHA-512, then hands that digest to
/// ring's `RSA_PKCS1_SHA512`, which hashes again before signing, giving
/// `SHA512(SHA512(payload))`. Getting this wrong produces an error from Swish that says nothing
/// about signatures, which is most of why this crate exists.
pub fn sign_payload(payload: &str, key_pair: &RsaKeyPair) -> Result<String> {
    let pre_hash = digest(&SHA512, payload.as_bytes());

    let rng = SystemRandom::new();
    let mut sig = vec![0u8; key_pair.public().modulus_len()];
    key_pair
        .sign(&RSA_PKCS1_SHA512, &rng, pre_hash.as_ref(), &mut sig)
        .map_err(|e| err(format!("RSA signing failed: {e}")))?;

    Ok(base64::encode(&sig))
}

/// Strips PEM armor and decodes the base64 body.
pub fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::decode(&b64).map_err(|e| err(format!("signing key PEM base64 is invalid: {e}")))
}

/// Parses a PEM private key.
///
/// ring reads PKCS#8 only, and plenty of tooling still emits PKCS#1: the same key in a different
/// wrapper. A PKCS#1 key is refused with the `openssl pkcs8 -topk8` command that converts it,
/// rather than ring's bare `InvalidEncoding`.
pub fn rsa_key_pair(key_pem: &str) -> Result<RsaKeyPair> {
    let der = pem_to_der(key_pem)?;
    RsaKeyPair::from_pkcs8(&der).map_err(|e| {
        if key_pem.contains("BEGIN RSA PRIVATE KEY") {
            err("the signing key is in PKCS#1 format and swisha needs PKCS#8. Convert it with: \
                 openssl pkcs8 -topk8 -nocrypt -in swish.key -out swish.pkcs8.key")
        } else {
            err(format!("the signing key is not a usable RSA key: {e}"))
        }
    })
}

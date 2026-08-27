//! Talking to Swish.
//!
//! The one rule that shapes this module: [`submit`] holds the crate's only `POST` to Swish, and
//! it has exactly one call site. [`reconcile`] recovers stalled payouts by asking Swish what
//! happened, never by sending anything.

pub mod cert;
pub mod client;
pub mod payload;
pub mod reconcile;
pub mod sign;
pub mod submit;

/// A fresh `payoutInstructionUUID`: 32 uppercase hex characters, UUID v4 shaped.
///
/// Swish treats this as the identity of a payout instruction, so a new one is a **new
/// instruction**. That is precisely why swisha never generates a second one for a reference it
/// has already submitted: Swish could not tie the two together, and would debit twice.
///
/// Six of the 128 bits are fixed by the version and variant markers, leaving 122 random.
pub fn random_payout_uuid() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    SystemRandom::new().fill(&mut bytes).expect("SystemRandom::fill failed");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    crate::util::hex::upper(&bytes)
}

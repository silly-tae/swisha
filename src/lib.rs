#![forbid(unsafe_code)]
// A published crate's docs.rs page is its front door, so an undocumented public item is a
// build failure rather than a warning nobody reads.
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
//! Swish payouts as a service, and as a library.
//!
//! swisha speaks the Swish CPC Payouts protocol: it signs the payload the way the reference
//! requires, refuses to pay the same reference twice, and recovers the outcome when a callback
//! never arrives. The binary adds an HTTP API on top; without the `http` feature you get the
//! payout engine, the storage trait and the Swish client on their own.
//!
//! # The guarantee that shapes everything else
//!
//! **swisha never resubmits a payout.** A resubmission needs a fresh `payoutInstructionUUID`,
//! which Swish cannot tie back to the original, so a payout that was in fact already debited
//! would be debited a second time. Recovery therefore only ever reads: it polls, and it asks
//! Swish what became of payouts that stall. Deciding to pay again is a person's call, and it is
//! made by issuing a new payout under a new reference.
//!
//! # Storage
//!
//! PostgreSQL, reached through the [`store::PayoutStore`] trait. The trait is defined by what
//! each operation must guarantee rather than by SQL, so another engine can satisfy it;
//! [`store::conformance`] is that contract as executable checks.
//!
//! ```toml
//! # Not on crates.io. Pin a tag rather than tracking a branch: an unpinned dependency that
//! # moves money is one `cargo update` away from a payout path you have not read.
//! swisha = { git = "https://github.com/silly-tae/swisha", tag = "v0.1.0", default-features = false }
//! ```
//!
//! # Where to start
//!
//! | Want to | Look at |
//! |---|---|
//! | Store payouts somewhere else | [`store::PayoutStore`], then [`store::conformance`] |
//! | Understand the state machine | [`domain::status`] |
//! | Turn a Swish error code into something a person can act on | [`domain::errors`] |
//! | Build a payout payload and sign it | [`swish::payload`], [`swish::sign`] |
//! | Read configuration from the environment | [`config::Config::from_env`] |
//!
//! The [README](https://github.com/silly-tae/swisha) covers running the service: certificates,
//! the reverse proxy, the callback allowlist and deployment.

pub mod backend;
pub mod config;
pub mod domain;
pub mod env;
pub mod error;
pub mod events;
#[cfg(feature = "http")]
pub mod http;
pub mod notify;
pub mod state;
pub mod store;
pub mod swish;
// Public because the integration tests reach into it, and an integration test can only see
// public items. Not API: hidden from the docs and exempt from semver.
#[doc(hidden)]
pub mod util;

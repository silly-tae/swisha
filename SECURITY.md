# Security policy

swisha moves money. A defect here does not corrupt data, it pays someone, so security reports
are treated as the highest priority work in the project.

## Reporting a vulnerability

**Do not open a public issue.**

Use GitHub's private vulnerability reporting: the **Security** tab on this repository, then
**Report a vulnerability**. If that is unavailable to you, email **dev@calia.cc** with `swisha
security` in the subject.

Useful in a report, in rough order of value:

- What an attacker gains. A double payout, a payout to the wrong recipient and a leak of personal
  data are all more serious than a crash.
- The smallest sequence of requests or configuration that reproduces it.
- Which version, which storage backend, and whether the internal API was on a Unix socket, a
  loopback port or a public address.

**Test against the MSS simulator, never against production Swish.** Set `SWISH_ENV=test` and use
the test certificates from Swish's developer portal. No money moves there.

You will get an acknowledgement within seven days. If the report is confirmed, expect a fix or a
documented mitigation before anything is disclosed publicly, and credit in the release notes
unless you would rather not have it. Please allow 90 days before disclosing, or less if a fix
ships sooner.

## Supported versions

swisha is pre-1.0. Only the latest release is supported, and fixes are not backported.

## What swisha guards

These are the properties a report can be written against. If you can break one, that is a
vulnerability.

| Property | How |
|---|---|
| One reference produces at most one payout | An atomic claim on the reference before anything is sent |
| No payout is ever resubmitted | A single submission call site, asserted by a test that scans the source |
| A settled payout cannot be un-settled | Nothing moves `DEBITED` or `PAID` back to `ERROR`, `DECLINED` or anything else. The one exception is forward: `DEBITED` accepts the `PAID` Swish sends seconds later |
| A repeat request cannot change an amount or a recipient | Every status locks the stored fields |
| The payout endpoint is never unguarded | swisha refuses to start on a public address without a shared secret |
| The shared secret does not leak through timing | Compared byte by byte with no early exit once the lengths match |
| A forged callback cannot settle a payout | Swish's addresses in production, plus the stored instruction UUID, which carries 122 bits of entropy |
| Swish's identity is verified | rustls with the system or supplied trust store. Certificate verification is never relaxed |
| A mismatched signing pair fails at startup | Checked on boot rather than on the first real payout |
| No memory-safety defects | `#![forbid(unsafe_code)]`, and no unsafe in any dependency swisha calls directly |

The tests that hold these are in `tests/`: `security.rs`, `safety.rs`, `no_resubmission.rs`,
`failure.rs` and `multi_instance.rs`.

## What the operator is responsible for

swisha cannot enforce these from inside the process. Getting one wrong is a real vulnerability in
a deployment, but it is not a defect in swisha.

- **The reverse proxy must overwrite `X-Forwarded-For`, not append to it.** The common nginx
  snippet appends, which lets a caller put a Swish address in the header and defeat the callback
  allowlist. The README shows the correct block and why.
- **Only the callback route may be public.** A prefix match instead of an exact one publishes the
  payout endpoint to the internet.
- **`TRUSTED_PROXY` must name the proxy**, or forwarded addresses are ignored and every genuine
  callback is refused.
- **Certificates and keys belong outside the web root**, readable only by the service account.
- **The `events` and `logs` event channels carry amounts and personal data.** Only `updates` is
  safe to forward to a browser.
- **Keep the shared secret out of version control.** The shipped `.gitignore` covers `*.env`,
  `*.pem`, `*.key`, `*.p12` and `*.csr` for that reason.

## Deliberate decisions that are not bugs

Reports about these are welcome as discussion, but they are choices rather than oversights.

- **swisha never retries a payout, automatically or otherwise.** A resubmission needs a fresh
  `payoutInstructionUUID`, which Swish cannot tie back to the original, so an already-debited
  payout could be debited again. Recovery only ever reads.
- **The event stream is lossy.** A subscriber that falls behind misses messages rather than
  slowing the payout path. Anything that must not be missed is read back from the status endpoint.
- **The rate limit is per process.** Running several instances behind a load balancer multiplies
  it. It is a guard against runaway callers, not a quota.
- **The Unix socket path is not a secret.** The permission bits are the authentication, and the
  kernel checks them before swisha sees a byte.
- **Outside production the callback allowlist logs but does not enforce.** This is what lets the
  MSS simulator and local tests work.

## Scope

In scope: this repository, and any deployment following the README.

Out of scope: vulnerabilities in Swish's own API or simulator, which belong to Swish; findings
that require an attacker to already have the shared secret, the signing key or write access to
the database; and denial of service through sheer request volume, which is the proxy's job.

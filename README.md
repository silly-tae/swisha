# swisha

A standalone service for Swish payouts. It speaks the Swish CPC Payouts protocol, refuses to pay
the same reference twice, and hands your application a small HTTP API.

It runs as its own process and you talk to it over HTTP, so the application calling it can be
written in anything. Rust is what swisha is built in, not something you need to write.

MIT · Rust 1.90 or newer · PostgreSQL · no `unsafe`, ten direct dependencies

---

## Why this exists

Swish's payout API is not the same as its payment API, and the parts that differ are the parts
that are easy to get wrong: the payload must carry a **double SHA-512** RSA signature, the
certificate that signs is not always the certificate that authenticates, and a rejected
signature comes back as an error that says nothing about signatures.

Most open examples are for payments rather than payouts, or predate the current API. swisha is a
working implementation with the awkward parts already solved and verified against Swish's own
simulator.

Payouts are business to consumer only. One Swish merchant number per instance; run one instance
per number if you have several.

## What it does

- **Signs correctly.** Double SHA-512 RSA, as the CPC payouts reference requires, verified end
  to end against the Swish MSS simulator.
- **Never pays twice.** A reference is claimed atomically before anything is sent, and no code
  path resubmits one. A test asserts the crate has exactly one call site that can submit a
  payout at all.
- **Recovers on its own.** Polls for the outcome when a callback never arrives, and asks Swish
  what became of payouts that stall. Recovery only ever reads; it never moves money.
- **Explains failures.** 27 Swish error responses mapped to a plain message and a category, in
  English or Swedish, so a person knows whether to pay again, ask the recipient to fix
  something, or escalate.
- **Runs on PostgreSQL**, behind a storage trait you can implement for anything else.
- **Contains no `unsafe` code.** The crate is `#![forbid(unsafe_code)]`.

## Quick start

Build:

```bash
git clone https://github.com/silly-tae/swisha && cd swisha
cargo build --release
```

Get the test certificates. Swish publishes a bundle on its
[developer portal](https://developer.swish.nu/documentation/environments); download it and
unpack it somewhere swisha can read. swisha names each role explicitly rather than inferring it
from a filename, so nothing needs renaming:

| From the bundle | Goes in |
|---|---|
| `Swish_Merchant_TestCertificate_<number>.pem` and `.key` | `SWISH_CERT`, `SWISH_KEY` |
| `Swish_Merchant_TestSigningCertificate_<number>.pem` and `.key` | `SWISH_SIGNING_CERT`, `SWISH_SIGNING_KEY` |
| `Swish_TLS_RootCA.pem` | `SWISH_CA` |

`<number>` is the test merchant number, `1234679304`. The bundle also ships `.p12` and `.csr`
copies of each pair; swisha reads the `.pem` and `.key`, so those two are all you need.

Both signing lines matter in test: MSS rejects a payout signed with the merchant key. Production
is simpler, usually two files where one pair does both jobs, so the signing lines stay blank.

Keys must be **PKCS#8**, the form that starts `-----BEGIN PRIVATE KEY-----`. If yours starts
`-----BEGIN RSA PRIVATE KEY-----` it is PKCS#1, which is the same key in a different wrapper:

```bash
openssl pkcs8 -topk8 -nocrypt -in swish.key -out swish.pkcs8.key
```

Copy a config template and fill in the fields marked `required` – the two database fields, the
merchant number, the callback URL, and the certificate paths above. Everything else is blank,
and blank means the default:

```bash
sudo mkdir -p /etc/swisha
sudo cp examples/swisha.dev.example /etc/swisha/dev.env
sudo chmod 600 /etc/swisha/dev.env
```

`/etc/swisha/` is where the rest of this README keeps configuration, and the file holds a
database password, so `600` from the start. Edit it in place with `sudo`.

The test merchant number is `1234679304`, and it is also the certificate's common name.
`SWISH_CALLBACK_URL` has to be set even locally. swisha polls for outcomes as well, so payouts
still resolve if Swish never reaches you; it just takes seconds rather than milliseconds. For a
real callback on your machine, point a tunnel at the callback listener:

```bash
cloudflared tunnel --url http://localhost:8084
```

Create the database and its tables. `--print-schema` writes the DDL to stdout and needs no
configuration and no connection of its own:

```bash
createdb swisha
./target/release/swisha --print-schema | psql swisha
```

Run it:

```bash
SWISHA_ENV_FILE=/etc/swisha/dev.env ./target/release/swisha
```

swisha does not search for that file, it opens exactly the path it is given. An absolute one
loads the file you meant from any working directory, and still works unchanged when you later
put it in a systemd unit.

Two listeners come up: the internal API on `127.0.0.1:8083` and the Swish callback on
`127.0.0.1:8084`. Send a payout:

```bash
curl -X POST http://127.0.0.1:8083/swish/payout \
  -H 'x-api-secret: <your secret>' \
  -H 'content-type: application/json' \
  -d '{
    "reference": "INV-1001",
    "payee_alias": "0701234567",
    "payee_ssn": "196408233234",
    "amount": 250.00
  }'
```

```json
{ "status": "CREATED", "success": true, "swish_ref": "7CBA599425BC499C93D6F0E7310085C3" }
```

The response is `202 Accepted`: Swish took the instruction, and the outcome is still to come. It
arrives on the event stream, and can always be read back from `/swish/status/INV-1001`.

Drop the `x-api-secret` header if you have not set `API_SHARED_SECRET`. On a loopback address a
secret is optional; anywhere else swisha refuses to start without one.

## How payouts work

swisha stores what its payout state machine needs and nothing else. Invoices, orders and line
items stay in your own tables, keyed by the same reference.

### The request

| Field | Required | Rules |
|---|---|---|
| `reference` | yes | Your identifier, opaque to swisha, sent to Swish as `payerPaymentReference`. Max 35 characters. Also the idempotency key |
| `payee_alias` | yes | The recipient's Swish number. Any format; normalized to `46XXXXXXXXX` |
| `payee_ssn` | no | Swedish personnummer, **12 digits** (`YYYYMMDDNNNN`), Luhn checked. Separators are ignored. Swish verifies it against the number. Omit it and the field is left out of the payload entirely, unless `SWISH_REQUIRE_SSN=true` |
| `amount` | yes | SEK. At least 1, at most `SWISH_MAX_PAYOUT` (default 50,000; Swish's own ceiling is 150,000) |
| `message` | no | Shown to the recipient in their Swish app. Truncated to 50 characters, which is Swish's limit. Defaults to the `SWISH_PAYOUT_MESSAGE` template |

Two rules that catch mistakes rather than passing them on:

**Unknown fields are refused**, not ignored. A caller sending an outdated shape gets a loud
error instead of a payout built from the fields that happened to match.

**A `payee_ssn` that is present but malformed is refused**, not dropped. A caller that supplied
an identity number asked Swish to check it against the phone number, and sending the payout
without it is not that check. Leaving the field out is fine; getting it wrong is not.

**`SWISH_REQUIRE_SSN=true` turns that into a guarantee of the instance.** Payouts are business
to consumer, so every recipient is a private individual and every payout can carry a
personnummer. With the setting on, one that does not is refused with `400` before anything
reaches Swish, whatever the caller believes it sent. It is per instance, so an app that always
holds the number can enforce it while another does not. Off by default, and a value that is
neither `true` nor `false` refuses to start rather than reading as off.

### The states

| Status | Meaning |
|---|---|
| `CREATED` | Claimed in the database. Nothing has been sent yet, or the outcome is not known |
| `PENDING` | Swish has the instruction and has not resolved it |
| `DEBITED` | The money has left the merchant account. Settled. Only `PAID` can follow |
| `PAID` | The recipient has the money. Swish's last word, and **final** |
| `DECLINED` | Swish refused the payout. No money moved |
| `ERROR` | Something failed. May be the submit, may be only the status lookup |
| `NEEDS_REVIEW` | swisha could not resolve it and has stopped chasing. A person decides |

Every payout starts at `CREATED` and moves once an answer arrives:

```text
CREATED ──┬──> DEBITED ──> PAID    settled
          ├──> DECLINED            Swish refused, no money moved
          ├──> ERROR               the outcome is not known
          └──> PENDING             Swish has it, no answer yet

CREATED, PENDING or ERROR ──> NEEDS_REVIEW    the sweep gave up
```

**A settled payout can never be un-settled.** Once a payout reaches `DEBITED` or `PAID`, no
callback moves it to `ERROR`, `DECLINED` or anything else, however late or duplicated. Every
other state can still change: a callback or a sweep that finally gets an answer settles a
`PENDING`, an `ERROR` or a `NEEDS_REVIEW` payout.

`NEEDS_REVIEW` therefore means swisha stopped asking, not that the payout is closed.

**One exception, and it is forward.** Swish reports a successful payout **twice**: `DEBITED` when
the money leaves your account, then `PAID` a few seconds later when the recipient has it, as two
separate callbacks. So `DEBITED` accepts `PAID`, and nothing else. `PAID` accepts nothing at all.

That matters if you consume the event stream: **a successful payout emits two status updates**,
not one. Treat `DEBITED` as sent and `PAID` as received rather than treating the first update you
see as the final answer.

### swisha never retries a payout

There is no automatic retry, and no manual one either. A resubmission needs a fresh
`payoutInstructionUUID`, which Swish cannot tie back to the original, so a payout that was in
fact already debited would be debited again. No guard removes that risk. Not resubmitting does.

What happens instead, in order:

1. The payout is submitted once. That is the crate's only `POST` to Swish.
2. swisha polls for the outcome, up to 8 times over roughly 48 seconds. A callback, if one
   arrives, settles it sooner.
3. If it is still unresolved, a sweep runs every 5 minutes and picks up anything that has not
   moved for 30 minutes. The sweep **asks Swish what happened** and writes down the answer. It
   never submits anything.
4. After 3 sweep attempts without an answer, the payout becomes `NEEDS_REVIEW` and swisha stops.

If a person then decides the money should go out, they issue a **new payout under a new
reference**. The error message and category tell them whether that is worth doing.

**A reference is spent the moment it is accepted.** Submitting it again returns `409`, whatever
state the payout reached, including `ERROR` and `DECLINED`. `ERROR` is the important case: it
can mean Swish accepted the instruction and only the status lookup failed, so resubmitting the
same reference could debit a second time. Treat `409` as "already handled, go and look", never
as "try again".

## API

Two listeners, deliberately separate. The callback has to be reachable from the internet; the
rest must not be.

| Listener | Default address | Setting | Endpoints |
|---|---|---|---|
| Internal | `127.0.0.1:8083` | `SWISH_SERVER_ADDR`, or `SWISH_SERVER_SOCKET` | payout, status, events, health |
| Callback | `127.0.0.1:8084` | `SWISH_CALLBACK_ADDR` | callback only |

Every internal endpoint takes the same authentication: the `x-api-secret` header, when a secret
is configured. See [Security](#security) for when one is required.

Errors on every endpoint share one shape:

```json
{ "error": "Payout already in progress for this reference. Swish ref: 7CBA5994..." }
```

| Code | When |
|---|---|
| `400` | The request is malformed: a bad field, an unknown field, a `payee_ssn` that is not 12 valid digits |
| `401` | Missing or wrong `x-api-secret` |
| `404` | No payout stored under that reference |
| `409` | The reference is already spent. **Never resubmit** |
| `429` | More than 30 payout requests in 60 seconds from one caller |
| `500` | An internal fault. The message is fixed, so no cause leaks to the caller |
| `503` | Swish rejected the payout, Swish is unreachable, or the database is |

### POST /swish/payout

Submits a payout. Returns `202 Accepted` once Swish has taken the instruction; the outcome
follows on the event stream.

```json
{ "status": "CREATED", "success": true, "swish_ref": "7CBA599425BC499C93D6F0E7310085C3" }
```

`swish_ref` is the `payoutInstructionUUID` swisha generated. It is what Swish knows the payout
by, and what you quote when asking them about it.

### GET /swish/status/{reference}

The current state of one payout. Shaped exactly like an `updates` event, so a client that
reconnects can feed it through the same handler.

```json
{
  "error_category": "user_fixable",
  "error_code": "ACMT07",
  "error_message": "The recipient is not enrolled in Swish. Check the Swish number.",
  "reference": "INV-1001",
  "status": "ERROR",
  "swish_ref": "7CBA599425BC499C93D6F0E7310085C3"
}
```

The three `error_*` fields are `null` while nothing has gone wrong. `error_category` is one of
`retryable`, `user_fixable` or `contact_support`, and `error_message` follows `SWISH_ERROR_LANG`
(`en` or `sv`). A code Swish adds later still resolves to a usable message rather than nothing.

### GET /events

A server-sent event stream. Both query parameters are optional.

```bash
curl -N --unix-socket /run/swisha/swisha.sock \
  'http://localhost/events?channel=updates&reference=INV-1001'
```

| Channel | Carries | Sensitive |
|---|---|---|
| `updates` | Status changes: the same object `/swish/status/{reference}` returns | no |
| `events` | The audit trail: adds `amount`, `payee_alias` and the caller's IP | **yes** |
| `logs` | Service log lines, with the payout reference and amount in their context | **yes** |

The SSE event name is the channel, so a client can switch on it directly:

```text
event: updates
data: {"error_category":null,"error_code":null,"error_message":null,"reference":"INV-1001","status":"PAID","swish_ref":"7CBA..."}
```

`events` and `logs` carry personal data and amounts. They are meant for an operator's own back
office, not for a customer-facing page. `updates` is the one to forward to a browser.

`reference=` matches only entries that carry one, so it filters `logs` out entirely.

The stream is fed in process, not from the database, so it works on every backend. Delivery is
lossy by design: a subscriber that falls behind misses messages rather than slowing payouts
down. Anything that must not be missed is read back from `/swish/status/{reference}`.

### GET /system/health

```json
{
  "db": true,
  "started_at": 1787821392,
  "status": "ok",
  "swish_checked_seconds_ago": 12,
  "swish_online": true,
  "timestamp": "2026-08-27T09:15:04+00:00",
  "version": "0.1.0"
}
```

The database is pinged live on every request, because a health check that reports a dead database
as healthy is worse than having none at all. Swish reachability is cached and refreshed every 30
seconds, since reaching them is slow and they rate-limit; `swish_checked_seconds_ago` says how
old the answer is, and is `null` if nothing has asked yet.

`status` is `degraded` when the database is down or Swish is known to be unreachable. An unknown
Swish answer is not degraded, because it only means no probe has run.

### POST /swish/callback

Where Swish reports the outcome. The only public endpoint, and the only one on the callback
listener. Not for your application to call.

Swish calls it **twice** for a successful payout, once for `DEBITED` and again for `PAID`. swisha
is idempotent about it: a repeat is recorded, a stale one is ignored, and a settled payout is
never un-settled.

What it answers, and why:

| Code | When |
|---|---|
| `200` | Handled, and also for anything swisha deliberately ignores: an unknown reference, a mismatched instruction UUID, a missing reference. Retrying those would never help |
| `403` | The source address is not Swish's, in production only |
| `500` | The database could not be read. The one case where swisha **wants** Swish to try again |

## Security

The payout endpoint moves money, so what guards it depends on where it listens. swisha refuses
to start in a configuration that leaves it unguarded.

[SECURITY.md](SECURITY.md) lists the properties swisha guarantees, what the operator has to get
right, and how to report a vulnerability. Please do not report one in a public issue.

| Listener | Guard | Secret |
|---|---|---|
| Unix socket | file permissions, enforced by the kernel | not needed |
| Loopback port | the host boundary | not needed, warns |
| Any other address | the shared secret | **required, refuses to boot without one** |

### Unix socket

The recommended deployment. Set `SWISH_SERVER_SOCKET` and swisha creates the socket with mode
`0660`, so access is a group membership rather than a string in a config file.

```console
$ ls -l /run/swisha/swisha.sock
srw-rw---- 1 swisha swisha 0 Aug 27 09:03 /run/swisha/swisha.sock
```

There is nothing inside it. A Unix socket is an address, not a file, and the permission bits are
the authentication: the kernel checks them at `connect()` before swisha sees a byte. The path is
not a secret and can be published.

Give the calling service that group and it can connect:

```bash
sudo usermod -aG swisha www-data
```

### Shared secret

For deployments where a socket will not do, such as another host or another container. Set
`API_SHARED_SECRET` and send it as `x-api-secret`. It is compared in constant time, must be at
least 16 characters, and is **not** a Swish credential. You generate it:

```bash
openssl rand -hex 32
```

### Certificates

Swish issues one certificate for production that serves both mTLS and payload signing, and a
separate signing pair for the test simulator. swisha names each role rather than guessing from
filenames, so you point it at the bundle as downloaded:

| Variable | Required | Notes |
|---|---|---|
| `SWISH_CERT`, `SWISH_KEY` | yes | mTLS. In production this pair also signs |
| `SWISH_SIGNING_CERT`, `SWISH_SIGNING_KEY` | no | Only when signing uses a different certificate. Set both or neither |
| `SWISH_CA` | no | Swish's root CA, needed for the simulator |

The serial is always read from whichever certificate signs. At startup swisha checks that the
signing certificate and the signing key are actually a pair, so a mismatched bundle fails on
boot rather than on the first payout.

### Reverse proxy and the callback allowlist

Swish requires HTTPS and swisha serves plain HTTP, so a reverse proxy is part of the deployment
rather than an option. Point it at the callback **and only the callback**.
`examples/nginx.conf.example` is this block as a file you can copy:

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name payouts.example.com;

    ssl_certificate     /etc/letsencrypt/live/payouts.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/payouts.example.com/privkey.pem;

    location = /swish/callback {
        proxy_pass http://127.0.0.1:8084;
        proxy_set_header Host              $host;
        proxy_set_header X-Forwarded-For   $remote_addr;   # overwrite, never append
        proxy_set_header X-Forwarded-Proto https;
    }

    location / { return 404; }
}
```

Three things there are load bearing.

**`location =`** is an exact match. A prefix match on `/swish/` would publish the payout endpoint
to the internet.

**`X-Forwarded-For $remote_addr`** overwrites whatever the caller sent. The usual snippet is
`$proxy_add_x_forwarded_for`, which *appends*, and swisha reads the first address in the list. A
caller who puts a Swish address in that header would be believed, which defeats the allowlist
below. Measured: with `$proxy_add_x_forwarded_for` a forged callback was accepted and marked a
payout `PAID`; with `$remote_addr` the same request was refused `403`.

**`TRUSTED_PROXY`** must name the proxy, or the forwarded address is ignored and every genuine
callback is refused instead. It defaults to `127.0.0.1`, which is right when the proxy is on the
same host.

When `SWISH_ENV=production`, callbacks are accepted only from Swish's eight published addresses.
Outside production the address is logged but not enforced, so the MSS simulator and local tests
work without pretending to be Swish.

The allowlist is the outer layer, not the only one. A callback also has to quote the stored
`payoutInstructionUUID`, so a forged one that gets past the address check still cannot settle a
payout without guessing 122 random bits.

### Rate limit

The payout endpoint allows 30 requests per caller per 60 seconds and answers `429` beyond that.
The limit is per process, so running several instances behind a load balancer multiplies it. Over
a Unix socket every caller shares one bucket, since the kernel has already decided who may
connect.

## Configuration

Every setting comes from the environment, or from a file named by `SWISHA_ENV_FILE`. The
environment wins, so systemd or `docker --env-file` can override a value without the file being
edited.

```bash
SWISHA_ENV_FILE=/etc/swisha/prod.env swisha
```

`SWISHA_ENV_FILE` is a plain filesystem path, with no search and no default location. **Use an
absolute path anywhere the working directory is not obviously yours** – a service, a container,
a cron entry. A relative path resolves against the process's working directory, which under
systemd is `/` unless the unit sets `WorkingDirectory=`, so a unit file naming `dev.env` looks
for `/dev.env` and fails to start.

Failing to start is the point. A file that is named but cannot be read is an error, never a
silent fallback to something else:

```text
Configuration error: Cannot read env file: /etc/swisha/prod.env: No such file or directory (os error 2)
```

A service told to load its configuration from somewhere should not come up with a different
configuration. Leaving `SWISHA_ENV_FILE` unset is the separate, deliberate case: no file is
looked for and every setting comes from the process environment, which is how you would run it
under Docker or Kubernetes.

`examples/swisha.dev.example` and `examples/swisha.prod.example` are the full list, with a
comment on every line.
Six settings are required, and swisha refuses to start without them:

| Variable | Notes |
|---|---|
| `DB_NAME` | The database |
| `DB_USER` | `DB_HOST` and `DB_PASS` have defaults; these two do not |
| `SWISH_NUMBER` | Your Swish merchant number. `1234679304` in the test bundle |
| `SWISH_CALLBACK_URL` | Where Swish reports outcomes. HTTPS, and required even in test |
| `SWISH_CERT`, `SWISH_KEY` | The mTLS pair. See [Certificates](#certificates) |

Two more are conditional:

| Variable | Required when |
|---|---|
| `SWISH_SIGNING_CERT`, `SWISH_SIGNING_KEY` | Signing uses a different certificate, which is the case in test. Set both or neither |
| `SWISH_REQUIRE_SSN` | Optional. `true` refuses any payout without a personnummer. Default `false` |
| `API_SHARED_SECRET` | The internal API listens on something other than a Unix socket or loopback |

Everything else has a default, and the templates ship those fields blank. A blank field means
the default rather than an empty string, which is what lets you copy a template, fill in the
lines marked `required`, and run.

`SWISH_ENV` is the one field the templates fill in for you: `test` in the dev template,
`production` in the prod one. Blank would mean `test`, which is the wrong default for a file
called prod.env and would leave the callback allowlist off.

A trailing `# comment` is cut from an unquoted value. Quote the value to keep a `#` inside it.

### Two environments

Keep one file per environment and select it with a symlink, so every value moves together:

```text
/etc/swisha/dev.env
/etc/swisha/prod.env
/etc/swisha/active.env -> prod.env
```

```ini
EnvironmentFile=/etc/swisha/active.env
```

```bash
ln -sfn dev.env /etc/swisha/active.env && systemctl restart swisha
```

More than the certificates differ between environments. The merchant number does too, and
switching one without the other submits payouts from the wrong account.

## Database

PostgreSQL, and only PostgreSQL. A payout service needs concurrent writers, and a single-writer
engine would cap a host at one instance.

Several instances can share one database. `TABLE_PAYOUTS`, `TABLE_LOGS`, `TABLE_EVENTS` and
`NOTIFY_PREFIX` namespace everything swisha touches, so one instance per Swish number, each with
its own tables and its own notification channels, runs side by side without collisions.

### Other engines

Storage sits behind a `PayoutStore` trait defined by what each operation must guarantee rather
than by SQL, so an engine without `RETURNING` or `ON CONFLICT` can satisfy it with a transaction.
`store::conformance` is that contract as executable checks, including concurrent claims and the
stall sweep's attempt bound. An adapter is supported when it passes.

## Using it as a library

Omit the `http` feature to get the payout engine, the storage trait and the Swish client with no
web framework attached:

```toml
[dependencies]
swisha = { git = "https://github.com/silly-tae/swisha", tag = "v0.1.0", default-features = false }
```

swisha is not on crates.io, so this is a git dependency. Pin a tag rather than tracking a
branch: an unpinned dependency that moves money is one `cargo update` away from a payout path
you have not read.

## Development

Every test needs a database. There is no in-memory fallback on purpose: the double-payout guard
is an `ON CONFLICT DO UPDATE ... RETURNING`, and testing it against anything but the real engine
would prove nothing.

```bash
docker compose -f examples/docker-compose.yml.example up -d

export SWISHA_TEST_DATABASE_URL=postgres://swisha:swisha@localhost:5433/swisha_test
cargo test
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```

`missing_docs` and broken intra-doc links are both denied, so an undocumented public item fails
the build rather than the docs. If `cargo check` complains about a missing doc comment, that is
why.

Each test creates its own tables under a unique prefix, using the same `TABLE_*` namespacing a
multi-instance deployment uses, so the suite runs in parallel against one server. Leftovers from
an interrupted run are swept at the start of the next one.

With `SWISHA_TEST_DATABASE_URL` unset the suite refuses to run rather than skipping. A payments
suite reporting `0 failed` while its guards never ran is worse than one that will not start.

With the test bundle from [Quick start](#quick-start) and `SWISH_ENV=test`, payouts go to the
MSS simulator, where no money moves.

## What is in `examples/`

Everything here is a working file rather than a sketch. The unit files parse, the nginx config
passes `nginx -t`, and the SQL runs against PostgreSQL.

| File | What it is |
|---|---|
| `swisha.dev.example` | Config template for the MSS simulator. No money moves |
| `swisha.prod.example` | Config template for production |
| `swisha.service.example` | systemd unit, one instance |
| `swisha@.service.example` | systemd template, one instance per Swish number |
| `nginx.conf.example` | The reverse proxy, publishing the callback and nothing else |
| `docker-compose.yml.example` | PostgreSQL for local development and the test suite |
| `setup.sql.example` | The role and database, before `--print-schema` makes the tables |

## References

- [Swish CPC API documentation](https://developer.swish.nu/)
- [Swish payouts API reference](https://developer.swish.nu/api/payouts/v1)
- [Swish test certificates and environment](https://developer.swish.nu/documentation/environments)

## License

MIT

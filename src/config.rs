//! Every setting swisha reads, and the guards that refuse an unsafe combination.
//!
//! [`Config::from_env`] is the only way a [`Config`] is built. It refuses to return one for a
//! configuration that would leave the payout endpoint unguarded, or that would sign with a key
//! that does not match its certificate.

use std::net::IpAddr;
use std::path::PathBuf;
use crate::env::Env;
use crate::error::{Context, Result, err};
use crate::domain::errors::Language;

/// Where the internal API listens.
///
/// A Unix socket is authenticated by the kernel from its file permissions, so it needs no
/// shared secret. A network address does, unless it is loopback.
#[derive(Clone, Debug)]
pub enum InternalListener {
    /// A network address, as `host:port`.
    Tcp(String),
    /// A Unix domain socket, created with mode `0660`.
    Unix(PathBuf),
}

impl InternalListener {
    /// A form suitable for logging, with socket paths prefixed `unix:`.
    pub fn describe(&self) -> String {
        match self {
            InternalListener::Tcp(addr) => addr.clone(),
            InternalListener::Unix(path) => format!("unix:{}", path.display()),
        }
    }
}

/// Everything swisha needs to run, resolved once at startup.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where the internal API listens.
    pub internal:             InternalListener,
    /// Where the public callback listener binds. `SWISH_CALLBACK_ADDR`.
    pub callback_addr:        String,
    /// Database host and port. `DB_HOST`.
    pub db_host:              String,
    /// Database name. `DB_NAME`, required.
    pub db_name:              String,
    /// Database user. `DB_USER`, required.
    pub db_user:              String,
    /// Database password. `DB_PASS`, blank for peer authentication.
    pub db_pass:              String,
    /// Payouts table. `TABLE_PAYOUTS`.
    pub table_payouts:        String,
    /// Service log table. `TABLE_LOGS`.
    pub table_logs:           String,
    /// Payout event table. `TABLE_EVENTS`.
    pub table_events:         String,
    /// Addresses whose `x-forwarded-for` and `x-real-ip` headers are believed. Anything else
    /// setting those headers is ignored, which is what keeps the callback allowlist meaningful.
    pub trusted_proxies:      Vec<IpAddr>,
    /// The `x-api-secret` value, when one is configured. Compared in constant time.
    pub api_secret:           Option<String>,
    /// `production` or `test`. Selects the Swish host and switches the callback allowlist on.
    pub swish_env:            String,
    /// The Swish host, resolved once from [`swish_env`](Self::swish_env).
    ///
    /// A field rather than a lookup per request, so a test can point the client at a local
    /// server without reaching for the network.
    pub swish_base_url:       String,
    /// The merchant's own Swish number, sent as `payerAlias`. `SWISH_NUMBER`, required.
    pub swish_number:         String,
    /// The largest payout swisha will accept, in SEK. Swish's own ceiling is 150,000.
    pub swish_max_payout:     f64,
    /// Where Swish reports outcomes. `SWISH_CALLBACK_URL`, required, and must be HTTPS.
    pub swish_callback_url:   String,
    /// The mTLS certificate, as PEM. `SWISH_CERT`, required.
    pub swish_tls_cert:       String,
    /// The mTLS private key, as PKCS#8 PEM. `SWISH_KEY`, required.
    pub swish_tls_key:        String,
    /// Swish's root CA, needed for the MSS simulator. `SWISH_CA`.
    pub swish_ca:             Option<String>,
    /// The key that signs payloads. Falls back to the mTLS key, which is how production works.
    pub swish_signing_key:    String,
    /// The serial of whichever certificate signs, which Swish uses to find the public key.
    pub swish_signing_serial: String,
    /// Template for the recipient's message, with `{reference}` substituted.
    pub payout_message:       String,
    /// Namespace for notification channels, so two services can share one database.
    pub notify_prefix:        String,
    /// Which language error messages are rendered in. `SWISH_ERROR_LANG`.
    pub error_language:       Language,
    /// Whether a payout must carry a personnummer. `SWISH_REQUIRE_SSN`, default false.
    ///
    /// Swish payouts are business to consumer, so every recipient is a private individual and
    /// every payout can carry one. Turning this on makes that a guarantee of the instance
    /// rather than a convention of whatever is calling it.
    pub require_ssn:          bool,
}

// Anything but true or false is refused rather than read as false. A guard that silently does
// not guard is worse than no guard: the operator believes payouts are verified and they are not.
fn parse_bool(key: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(err(format!("{key} must be 'true' or 'false', got: '{other}'"))),
    }
}

fn validate_identifier(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(err(format!("Invalid table name '{name}': must be alphanumeric with underscores only")));
    }
    Ok(())
}

// A socket path wins when both are set: it is the more restrictive of the two.
fn internal_listener(env: &Env) -> InternalListener {
    let socket = env.optional("SWISH_SERVER_SOCKET", "");
    if socket.trim().is_empty() {
        InternalListener::Tcp(env.optional("SWISH_SERVER_ADDR", "127.0.0.1:8083"))
    } else {
        InternalListener::Unix(PathBuf::from(socket.trim()))
    }
}

fn is_loopback(addr: &str) -> bool {
    addr.rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(addr)
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn parse_trusted_proxies(s: &str) -> Vec<IpAddr> {
    s.split(',').map(str::trim).filter_map(|ip| ip.parse().ok()).collect()
}

fn read_cert_file(path: &str) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read cert file: {path}"))
}

struct CertBundle {
    tls_cert:       String,
    tls_key:        String,
    ca:             Option<String>,
    signing_key:    String,
    signing_serial: String,
}

// Swish issues one certificate for production that serves both mTLS and payload signing,
// while the test bundle ships a separate signing pair that MSS insists on. Rather than infer
// which is which from filenames, each role is named explicitly and signing falls back to the
// mTLS pair, so production needs only two paths.
fn load_certs(env: &Env) -> Result<CertBundle> {
    let tls_cert = read_cert_file(&env.required("SWISH_CERT")?)?;
    let tls_key = read_cert_file(&env.required("SWISH_KEY")?)?;

    let signing_cert_path = env.optional("SWISH_SIGNING_CERT", "");
    let signing_key_path = env.optional("SWISH_SIGNING_KEY", "");

    // Half a signing pair means a cert from one pair and a key from another, which produces a
    // signature Swish cannot verify and an error that says nothing about the cause.
    if signing_cert_path.trim().is_empty() != signing_key_path.trim().is_empty() {
        return Err(err(
            "SWISH_SIGNING_CERT and SWISH_SIGNING_KEY must be set together, or neither: a certificate paired with the wrong key produces signatures Swish will reject",
        ));
    }

    let (signing_cert, signing_key) = if signing_cert_path.trim().is_empty() {
        (tls_cert.clone(), tls_key.clone())
    } else {
        (
            read_cert_file(&signing_cert_path)?,
            read_cert_file(&signing_key_path)?,
        )
    };

    let ca = match env.optional("SWISH_CA", "") {
        path if path.trim().is_empty() => None,
        path => Some(read_cert_file(&path)?),
    };

    // Always taken from whichever certificate actually signs, never from the mTLS one.
    let signing_serial = crate::swish::cert::read_cert_serial(&signing_cert)?;

    // Refused here rather than discovered on the first real payout in production.
    if !crate::swish::cert::certificate_matches_key(&signing_cert, &signing_key)? {
        return Err(err(
            "SWISH_SIGNING_CERT and SWISH_SIGNING_KEY are not a pair. Swish signs with the key \
             and identifies it by the certificate's serial, so a mismatch is rejected as PA01 on \
             every payout. Check that both point at the same certificate's files.",
        ));
    }

    Ok(CertBundle {
        tls_cert,
        tls_key,
        ca,
        signing_key,
        signing_serial,
    })
}

impl Config {
    /// Reads every setting, applies defaults, and refuses an unsafe combination.
    ///
    /// # Errors
    ///
    /// Refuses to return a config when a required setting is missing (`DB_NAME`, `DB_USER`,
    /// `SWISH_NUMBER`, `SWISH_CALLBACK_URL`, `SWISH_CERT`, `SWISH_KEY`), when the payout
    /// endpoint would listen on a non-loopback address with no `API_SHARED_SECRET`, when a
    /// secret is shorter than 16 characters, when the two listeners share an address, when a
    /// table name is not a plain identifier, when only half a signing pair is set, or when the
    /// signing certificate and key are not actually a pair.
    pub fn from_env(env: &Env) -> Result<Self> {
        let swish_env = env.optional("SWISH_ENV", "test");

        // Rejected before the certificates load, so a typo fails on the typo.
        match swish_env.as_str() {
            "production" | "test" => {}
            other => return Err(err(format!("SWISH_ENV must be 'production' or 'test', got: '{other}'"))),
        }

        let certs = load_certs(env)?;

        let swish_number       = env.optional("SWISH_NUMBER",       "");
        let swish_callback_url = env.optional("SWISH_CALLBACK_URL", "");

        // Without these a payout cannot be addressed or reported on, so refuse to start.
        if swish_number.is_empty() {
            return Err(err("SWISH_NUMBER is required"));
        }
        if swish_callback_url.is_empty() {
            return Err(err("SWISH_CALLBACK_URL is required"));
        }

        let config = Self {
            internal:       internal_listener(env),
            callback_addr:  env.optional("SWISH_CALLBACK_ADDR", "127.0.0.1:8084"),
            db_host:        env.optional("DB_HOST",        "localhost"),
            db_name:        env.required("DB_NAME")?,
            db_user:        env.required("DB_USER")?,
            db_pass: {
                let p = env.optional("DB_PASS", "");
                if p.is_empty() { eprintln!("Warning: DB_PASS is not set – connecting without a password"); }
                p
            },
            table_payouts:  env.optional("TABLE_PAYOUTS", "swisha_payouts"),
            table_logs:     env.optional("TABLE_LOGS",    "swisha_logs"),
            table_events:   env.optional("TABLE_EVENTS",  "swisha_events"),
            trusted_proxies:  parse_trusted_proxies(&env.optional("TRUSTED_PROXY", "127.0.0.1")),
            api_secret: {
                let secret = env.optional("API_SHARED_SECRET", "");
                (!secret.trim().is_empty()).then_some(secret)
            },
            swish_base_url: crate::swish::client::swish_base_url(&swish_env).to_string(),
            swish_env,
            swish_number,
            swish_max_payout: env
                .get("SWISH_MAX_PAYOUT")
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(50_000.0),
            swish_callback_url,
            swish_tls_cert:     certs.tls_cert,
            swish_tls_key:      certs.tls_key,
            swish_ca:           certs.ca,
            swish_signing_key:  certs.signing_key,
            swish_signing_serial: certs.signing_serial,
            payout_message:   env.optional("SWISH_PAYOUT_MESSAGE", "{reference}"),
            notify_prefix:    env.optional("NOTIFY_PREFIX", "swisha"),
            error_language:   Language::parse(&env.optional("SWISH_ERROR_LANG", "en")),
            require_ssn:      parse_bool("SWISH_REQUIRE_SSN", &env.optional("SWISH_REQUIRE_SSN", "false"))?,
        };
        // A secret that exists must be strong enough to be worth having.
        if let Some(secret) = &config.api_secret
            && secret.len() < 16
        {
            return Err(err(
                "API_SHARED_SECRET must be at least 16 characters. Generate one with: openssl rand -hex 32",
            ));
        }

        // What guards the payout endpoint depends on where it listens. A Unix socket is
        // guarded by its file permissions; a loopback port by the host boundary; a network
        // port by nothing at all unless a secret is set.
        match (&config.internal, &config.api_secret) {
            (InternalListener::Unix(_), _) => {}
            (InternalListener::Tcp(addr), None) if is_loopback(addr) => {
                eprintln!(
                    "Warning: API_SHARED_SECRET is not set. {addr} is loopback, so only processes on this host can reach the payout endpoint, but any of them can."
                );
            }
            (InternalListener::Tcp(addr), None) => {
                return Err(err(format!(
                    "API_SHARED_SECRET is required when the payout endpoint listens on {addr}: a non-loopback address is reachable from the network. Set a secret, bind loopback, or use SWISH_SERVER_SOCKET."
                )));
            }
            (InternalListener::Tcp(_), Some(_)) => {}
        }

        if let InternalListener::Tcp(addr) = &config.internal
            && addr == &config.callback_addr
        {
            return Err(err(
                "SWISH_SERVER_ADDR and SWISH_CALLBACK_ADDR must differ: the payout endpoint must not share a listener with the publicly reachable callback",
            ));
        }
        validate_identifier(&config.notify_prefix)?;
        validate_identifier(&config.table_payouts)?;
        validate_identifier(&config.table_logs)?;
        validate_identifier(&config.table_events)?;
        Ok(config)
    }
}

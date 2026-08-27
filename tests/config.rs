// config.rs is 241 lines that decide what guards the payout endpoint, and it had no test file.
// The guard matrix was verified by hand once, on one machine, and never again.

use std::collections::BTreeMap;
use std::path::PathBuf;

use swisha::config::{Config, InternalListener};
use swisha::env::{Env, EnvFile};
use swisha::error::Result;

// A real certificate and its own key, generated once per run. The signing pair is verified at
// startup, so a certificate standing in for a key no longer loads, and rightly so.
fn pair() -> &'static (String, String) {
    static PAIR: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    PAIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!(
            "swisha-cfg-{}",
            swisha::swish::random_payout_uuid()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let key = dir.join("test.key");
        let crt = dir.join("test.crt");
        let out = std::process::Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=test"])
            .arg("-keyout").arg(&key)
            .arg("-out").arg(&crt)
            .output()
            .expect("openssl is required to generate a throwaway pair");
        assert!(out.status.success(), "openssl req failed");
        (
            crt.to_string_lossy().to_string(),
            key.to_string_lossy().to_string(),
        )
    })
}

fn base() -> BTreeMap<&'static str, String> {
    let mut m = BTreeMap::new();
    for (k, v) in [
        ("DB_NAME", "swisha"),
        ("DB_USER", "swisha"),
        ("SWISH_NUMBER", "1234679304"),
        ("SWISH_CALLBACK_URL", "https://example.test/swish/callback"),
        ("SWISH_CERT", pair().0.as_str()),
        ("SWISH_KEY", pair().1.as_str()),
        ("SWISH_SIGNING_CERT", pair().0.as_str()),
        ("SWISH_SIGNING_KEY", pair().1.as_str()),
    ] {
        m.insert(k, v.to_string());
    }
    m
}

// A real file on disk, because EnvFile reads one and pretending otherwise would test a
// different loader than the service uses.
fn load(settings: &BTreeMap<&'static str, String>) -> Result<Config> {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "swisha-config-{}.env",
        swisha::swish::random_payout_uuid()
    ));
    let body: String = settings
        .iter()
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect();
    std::fs::write(&path, body).expect("write env file");
    let result = EnvFile::load(&path).and_then(|f| Config::from_env(&Env::with_file(f)));
    let _ = std::fs::remove_file(&path);
    result
}

fn with(pairs: &[(&'static str, &str)]) -> Result<Config> {
    let mut s = base();
    for (k, v) in pairs {
        s.insert(k, v.to_string());
    }
    load(&s)
}

#[test]
fn a_minimal_configuration_loads_with_every_default_applied() {
    let c = with(&[]).expect("minimal config should load");
    assert!(matches!(c.internal, InternalListener::Tcp(ref a) if a == "127.0.0.1:8083"));
    assert_eq!(c.callback_addr, "127.0.0.1:8084");
    assert_eq!(c.table_payouts, "swisha_payouts");
    assert_eq!(c.notify_prefix, "swisha");
    assert_eq!(c.swish_env, "test");
    assert_eq!(c.swish_max_payout, 50_000.0);
    assert!(c.api_secret.is_none());
}

// What guards the payout endpoint depends entirely on where it listens, and getting this wrong
// puts a money-moving endpoint on a network with nothing in front of it.
#[test]
fn a_unix_socket_needs_no_secret() {
    let c = with(&[("SWISH_SERVER_SOCKET", "/tmp/swisha-test.sock")])
        .expect("a socket is guarded by its file permissions");
    assert!(matches!(c.internal, InternalListener::Unix(_)));
    assert!(c.api_secret.is_none());
}

#[test]
fn a_socket_takes_precedence_over_an_address() {
    let c = with(&[
        ("SWISH_SERVER_SOCKET", "/tmp/swisha-test.sock"),
        ("SWISH_SERVER_ADDR", "0.0.0.0:8083"),
    ])
    .expect("socket wins");
    assert!(matches!(c.internal, InternalListener::Unix(_)));
}

#[test]
fn loopback_without_a_secret_is_allowed() {
    for addr in ["127.0.0.1:8083", "127.0.0.5:8083", "[::1]:8083"] {
        assert!(
            with(&[("SWISH_SERVER_ADDR", addr)]).is_ok(),
            "{addr} is loopback and should not require a secret"
        );
    }
}

// Only a literal address counts. A name resolves through /etc/hosts and DNS, so it can point
// anywhere, and treating one as trusted would hand that decision to whoever controls
// resolution. Demanding a secret is the wrong-way-safe answer.
#[test]
fn a_hostname_is_never_assumed_to_be_loopback() {
    assert!(
        with(&[("SWISH_SERVER_ADDR", "localhost:8083")]).is_err(),
        "localhost is a name, not an address"
    );
    assert!(with(&[
        ("SWISH_SERVER_ADDR", "localhost:8083"),
        ("API_SHARED_SECRET", "0123456789abcdef"),
    ])
    .is_ok(), "with a secret it is fine, wherever it resolves");
}

#[test]
fn a_network_address_without_a_secret_refuses_to_start() {
    for addr in ["0.0.0.0:8083", "192.168.1.10:8083", "10.0.0.5:8083"] {
        let err = with(&[("SWISH_SERVER_ADDR", addr)])
            .expect_err(&format!("{addr} is reachable from the network"))
            .to_string();
        assert!(
            err.contains("API_SHARED_SECRET"),
            "the refusal should name what is missing: {err}"
        );
    }
}

#[test]
fn a_network_address_with_a_secret_is_accepted() {
    let c = with(&[
        ("SWISH_SERVER_ADDR", "0.0.0.0:8083"),
        ("API_SHARED_SECRET", "0123456789abcdef"),
    ])
    .expect("a secret is what makes a network bind acceptable");
    assert_eq!(c.api_secret.as_deref(), Some("0123456789abcdef"));
}

// A short secret is worse than none: it reads as protection while being guessable.
#[test]
fn a_secret_shorter_than_sixteen_characters_refuses_to_start() {
    let err = with(&[("API_SHARED_SECRET", "0123456789abcde")])
        .expect_err("fifteen characters is too short")
        .to_string();
    assert!(err.contains("16"), "{err}");

    assert!(
        with(&[("API_SHARED_SECRET", "0123456789abcdef")]).is_ok(),
        "sixteen is the documented minimum and must be accepted"
    );
}

#[test]
fn a_blank_secret_is_absent_rather_than_empty() {
    let c = with(&[("API_SHARED_SECRET", "   ")]).expect("blank means unset");
    assert!(c.api_secret.is_none(), "an empty secret must never authorize anyone");
}

// The two listeners exist so a proxy misconfiguration cannot expose the payout endpoint. Sharing
// one address would undo that quietly.
#[test]
fn the_two_listeners_may_not_share_an_address() {
    let err = with(&[
        ("SWISH_SERVER_ADDR", "127.0.0.1:9000"),
        ("SWISH_CALLBACK_ADDR", "127.0.0.1:9000"),
    ])
    .expect_err("one address cannot serve both")
    .to_string();
    assert!(err.contains("must differ"), "{err}");
}

// Table names are interpolated into SQL, so anything but an identifier has to be refused here.
#[test]
fn table_names_must_be_plain_identifiers() {
    for bad in ["payouts; DROP TABLE swisha_payouts", "payouts-1", "payouts table", "payouts'"] {
        assert!(
            with(&[("TABLE_PAYOUTS", bad)]).is_err(),
            "{bad:?} must not reach a query"
        );
    }
    assert!(with(&[("TABLE_PAYOUTS", "custom_payouts_2")]).is_ok());

    // Blank is not an invalid name, it is no name: the default applies, as it does everywhere.
    let c = with(&[("TABLE_PAYOUTS", "")]).expect("blank falls through to the default");
    assert_eq!(c.table_payouts, "swisha_payouts");
}

#[test]
fn a_signing_certificate_without_its_key_refuses_to_start() {
    let mut s = base();
    s.remove("SWISH_SIGNING_KEY");
    assert!(load(&s).is_err(), "half a signing pair is a misconfiguration");

    let mut s = base();
    s.remove("SWISH_SIGNING_CERT");
    assert!(load(&s).is_err());
}

#[test]
fn the_required_settings_are_actually_required() {
    for key in ["DB_NAME", "DB_USER", "SWISH_CERT", "SWISH_KEY", "SWISH_NUMBER"] {
        let mut s = base();
        s.remove(key);
        let err = load(&s).expect_err(&format!("{key} is required")).to_string();
        assert!(err.contains(key), "the error should name {key}: {err}");
    }
}

#[test]
fn an_unknown_swish_env_is_refused_before_the_certificates_load() {
    let err = with(&[("SWISH_ENV", "staging")])
        .expect_err("only test and production exist")
        .to_string();
    assert!(err.to_lowercase().contains("swish_env"), "{err}");
}

// Resolved once at startup, so a request never decides which Swish it is talking to.
#[test]
fn the_base_url_follows_the_environment() {
    assert_eq!(
        with(&[("SWISH_ENV", "test")]).expect("test").swish_base_url,
        "https://mss.cpc.getswish.net"
    );
    assert_eq!(
        with(&[("SWISH_ENV", "production")]).expect("production").swish_base_url,
        "https://cpc.getswish.net"
    );
}

#[test]
fn trusted_proxies_parse_into_a_list() {
    let c = with(&[("TRUSTED_PROXY", "127.0.0.1, 10.0.0.1")]).expect("config");
    assert_eq!(c.trusted_proxies.len(), 2);
}

// The mixup this guards against: four files with near-identical names, and the signing key
// pointed at the wrong one. It starts fine and Swish then rejects every payout as PA01, an error
// that names no key.
#[test]
fn a_signing_certificate_and_key_that_are_not_a_pair_refuse_to_start() {
    // A second, unrelated pair standing in for the wrong file being reached for.
    let dir = std::env::temp_dir().join(format!("swisha-other-{}", swisha::swish::random_payout_uuid()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let other_key = dir.join("other.key");
    let other_crt = dir.join("other.crt");
    let out = std::process::Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=other"])
        .arg("-keyout").arg(&other_key)
        .arg("-out").arg(&other_crt)
        .output()
        .expect("openssl");
    assert!(out.status.success());

    let err = with(&[("SWISH_SIGNING_KEY", other_key.to_string_lossy().as_ref())])
        .expect_err("a mismatched signing pair must not start")
        .to_string();
    assert!(err.contains("not a pair"), "the refusal should say why: {err}");

    let _ = std::fs::remove_dir_all(&dir);
}

// The production shape. Swish's test bundle ships a separate signing pair, but a bank issuing
// production certificates hands over one certificate and one key that serve both roles: the mTLS
// identity and the payload signature. Two files, not four.
#[test]
fn one_certificate_may_serve_as_both_identity_and_signer() {
    let mut settings = base();
    settings.remove("SWISH_SIGNING_CERT");
    settings.remove("SWISH_SIGNING_KEY");

    let c = load(&settings).expect("two files is a complete production configuration");
    assert!(
        !c.swish_signing_serial.is_empty(),
        "the serial has to come from the merchant certificate when there is no separate one"
    );

    // And it is that certificate's serial, not something else's.
    let expected = swisha::swish::cert::read_cert_serial(
        &std::fs::read_to_string(pair().0.as_str()).expect("read certificate"),
    )
    .expect("serial");
    assert_eq!(c.swish_signing_serial, expected);
}

// The pairing guard must not misfire on that shape: falling back to the mTLS pair is still a
// pair, so it has to pass.
#[test]
fn the_pairing_guard_accepts_the_two_file_shape() {
    let mut settings = base();
    settings.remove("SWISH_SIGNING_CERT");
    settings.remove("SWISH_SIGNING_KEY");
    assert!(load(&settings).is_ok(), "the guard must not reject a production layout");
}

// The templates say which fields have to be filled in, and the README repeats them. That claim
// used to be wrong: it said four when the code demanded six, so a newcomer got two failed starts.
// Reading the markers back out of the shipped file is the only way it stays true.
fn keys_marked_required(template: &str) -> Vec<String> {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(template),
    )
    .unwrap_or_else(|e| panic!("{template}: {e}"))
    .lines()
    .filter_map(|line| {
        let (key, comment) = line.split_once('=')?;
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            return None;
        }
        comment.contains("# required").then(|| key.to_string())
    })
    .collect()
}

#[test]
fn every_field_the_templates_mark_required_really_is() {
    for template in ["examples/swisha.dev.example", "examples/swisha.prod.example"] {
        let marked = keys_marked_required(template);
        assert!(!marked.is_empty(), "{template}: nothing is marked required");

        for key in &marked {
            // The signing pair is the one conditional case: blank means fall back to the mTLS
            // pair, which loads fine. Test needs it because MSS refuses the merchant key, and
            // that is a Swish rule swisha cannot enforce at startup.
            if key.starts_with("SWISH_SIGNING_") {
                continue;
            }
            let mut settings = base();
            settings.remove(key.as_str());
            let error = load(&settings)
                .err()
                .unwrap_or_else(|| panic!("{template}: {key} is marked required but loads without it"))
                .to_string();
            assert!(
                error.contains(key.as_str()),
                "{template}: dropping {key} failed with a message that never names it: {error}"
            );
        }
    }
}

#[test]
fn nothing_the_templates_leave_blank_is_secretly_required() {
    for template in ["examples/swisha.dev.example", "examples/swisha.prod.example"] {
        let marked = keys_marked_required(template);
        let mut settings = base();

        // Everything the template does not mark is stripped back to its default, which is what a
        // reader who filled in only the marked lines actually ends up running.
        settings.retain(|key, _| marked.iter().any(|m| m == key));
        assert!(
            load(&settings).is_ok(),
            "{template}: filling in only the fields marked required does not start"
        );
    }
}

// SWISH_REQUIRE_SSN is a guard, so a value it does not understand has to stop the service. A
// flag that silently reads as false is worse than no flag: the operator believes every payout
// is verified against a personnummer, and none of them are.
#[test]
fn require_ssn_is_off_unless_explicitly_turned_on() {
    assert!(!with(&[]).expect("default config").require_ssn, "absent must mean off");
    assert!(
        !with(&[("SWISH_REQUIRE_SSN", "")]).expect("blank").require_ssn,
        "blank must mean off, the way every other template field does"
    );
    assert!(!with(&[("SWISH_REQUIRE_SSN", "false")]).expect("false").require_ssn);
    assert!(with(&[("SWISH_REQUIRE_SSN", "true")]).expect("true").require_ssn);
    assert!(with(&[("SWISH_REQUIRE_SSN", "TRUE")]).expect("TRUE").require_ssn, "case insensitive");
    assert!(with(&[("SWISH_REQUIRE_SSN", " true ")]).expect("padded").require_ssn, "trimmed");
}

#[test]
fn a_value_that_is_neither_true_nor_false_refuses_to_start() {
    for wrong in ["yes", "1", "on", "enabled", "maybe"] {
        let error = with(&[("SWISH_REQUIRE_SSN", wrong)])
            .err()
            .unwrap_or_else(|| panic!("'{wrong}' was accepted instead of refused"))
            .to_string();
        assert!(
            error.contains("SWISH_REQUIRE_SSN") && error.contains(wrong),
            "the error must name the setting and the value: {error}"
        );
    }
}

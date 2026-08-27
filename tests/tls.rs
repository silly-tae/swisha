// Polling is how a payout's outcome is decided, so the connection that carries the answer has to
// be authenticated. If swisha could be made to trust an impostor, anything on the network path
// could answer "PAID" for a payout that never happened.
//
// Opt-in, because it needs a server standing in for Swish with a certificate nothing trusts:
//
//   SWISHA_TEST_IMPOSTOR=https://127.0.0.1:8470 \
//   SWISHA_TEST_IMPOSTOR_CA=/path/to/fake.crt \
//   SWISH_TEST_CERT_DIR=~/Downloads/client_cert \
//     cargo test --test tls -- --nocapture

use swisha::swish::client::{build_swish_client, swish_get};

fn config_with_ca(ca: Option<String>) -> Option<swisha::config::Config> {
    let dir = std::env::var("SWISH_TEST_CERT_DIR").ok()?;
    let read = |f: &str| std::fs::read_to_string(format!("{dir}/{f}")).ok();
    let mut config = swisha::config::Config {
        internal: swisha::config::InternalListener::Tcp("127.0.0.1:0".into()),
        callback_addr: "127.0.0.1:0".into(),
        db_host: "127.0.0.1".into(),
        db_name: ":memory:".into(),
        db_user: "swisha".into(),
        db_pass: String::new(),
        table_payouts: "swisha_payouts".into(),
        table_logs: "swisha_logs".into(),
        table_events: "swisha_events".into(),
        trusted_proxies: Vec::new(),
        api_secret: None,
        swish_env: "test".into(),
        swish_base_url: "https://mss.cpc.getswish.net".into(),
        swish_number: "1234679304".into(),
        swish_max_payout: 50_000.0,
        swish_callback_url: "https://example.test/swish/callback".into(),
        swish_tls_cert: read("Swish_Merchant_TestCertificate_1234679304.pem")?,
        swish_tls_key: read("Swish_Merchant_TestCertificate_1234679304.key")?,
        swish_ca: None,
        swish_signing_key: String::new(),
        swish_signing_serial: "00".into(),
        payout_message: "{reference}".into(),
        notify_prefix: "swisha".into(),
        error_language: swisha::domain::errors::Language::English,
        require_ssn: false,
    };
    config.swish_ca = ca;
    Some(config)
}

fn impostor() -> Option<String> {
    std::env::var("SWISHA_TEST_IMPOSTOR").ok()
}

// The attack: something on the network answers as Swish, with its own certificate.
#[tokio::test]
async fn swisha_refuses_a_server_impersonating_swish() {
    let (Some(url), Some(config)) = (impostor(), config_with_ca(None)) else {
        eprintln!("skipped: SWISHA_TEST_IMPOSTOR and SWISH_TEST_CERT_DIR are not both set");
        return;
    };
    let client = build_swish_client(&config).expect("client");

    let result = swish_get(&format!("{url}/swish-cpcapi/api/v1/payouts/ABC"), &client).await;
    assert!(
        result.is_err(),
        "swisha accepted an untrusted certificate and would have believed its answer"
    );
    let message = result.unwrap_err().to_string();
    eprintln!("   refused with: {message}");
}

// The control. Without it, the refusal above could just mean the server was unreachable, and the
// test would pass for entirely the wrong reason.
#[tokio::test]
async fn the_same_server_is_reachable_once_its_certificate_is_trusted() {
    let Some(ca_path) = std::env::var("SWISHA_TEST_IMPOSTOR_CA").ok() else {
        eprintln!("skipped: SWISHA_TEST_IMPOSTOR_CA is not set");
        return;
    };
    let ca = std::fs::read_to_string(&ca_path).expect("read the impostor's certificate");
    let (Some(url), Some(config)) = (impostor(), config_with_ca(Some(ca))) else {
        eprintln!("skipped: SWISHA_TEST_IMPOSTOR and SWISH_TEST_CERT_DIR are not both set");
        return;
    };
    let client = build_swish_client(&config).expect("client");

    let result = swish_get(&format!("{url}/swish-cpcapi/api/v1/payouts/ABC"), &client).await;
    assert!(
        result.is_ok(),
        "the harness itself is broken: the server is unreachable even when trusted"
    );
    let body = result.unwrap().text().await.expect("body");
    assert!(body.contains("PAID"), "the impostor answers PAID, so the refusal above was real");
    eprintln!("   trusted, and it answered: {body}");
}

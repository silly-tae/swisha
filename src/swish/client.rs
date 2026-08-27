//! The mTLS HTTP client, and which Swish host to use.

use std::time::Duration;
use crate::error::{BoxError, Context, Result, err};
use crate::config::Config;

/// Builds the mTLS client used for every call to Swish.
///
/// The simulator is signed by Swish's own CA, so a test configuration adds it explicitly.
/// Production certificates chain to the system trust store.
pub fn build_swish_client(config: &Config) -> Result<reqwest::Client> {
    // Identity requires cert + key concatenated in a single PEM buffer
    let mut identity_pem = config.swish_tls_cert.clone();
    identity_pem.push('\n');
    identity_pem.push_str(&config.swish_tls_key);

    let identity = reqwest::Identity::from_pem(identity_pem.as_bytes())
        .map_err(|e| with_cause("Failed to read the Swish mTLS identity", &e))?;

    let mut builder = reqwest::Client::builder()
        .identity(identity)
        .timeout(Duration::from_secs(55));

    if let Some(ca_pem) = &config.swish_ca {
        let ca_cert = reqwest::Certificate::from_pem(ca_pem.as_bytes())
            .context("Failed to parse Swish test CA cert")?;
        builder = builder.add_root_certificate(ca_cert);
    }

    builder.build().map_err(|e| with_cause("Failed to build the Swish HTTPS client", &e))
}

// reqwest renders a builder failure as the bare words "builder error". The reason a certificate
// was refused sits one level down, so without the chain an operator has nothing to act on.
fn with_cause(prefix: &str, error: &dyn std::error::Error) -> BoxError {
    let mut message = format!("{prefix}: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    err(message)
}

/// The Swish host for an environment: production, or the MSS simulator for anything else.
pub fn swish_base_url(swish_env: &str) -> &'static str {
    if swish_env == "production" {
        "https://cpc.getswish.net"
    } else {
        "https://mss.cpc.getswish.net"
    }
}

/// A JSON POST to Swish.
pub async fn swish_post(url: &str, body: &impl serde::Serialize, client: &reqwest::Client) -> Result<reqwest::Response> {
    client.post(url)
        .json(body)
        .send()
        .await
        .context("Swish POST request failed")
}

/// A GET from Swish.
pub async fn swish_get(url: &str, client: &reqwest::Client) -> Result<reqwest::Response> {
    client.get(url)
        .send()
        .await
        .context("Swish GET request failed")
}

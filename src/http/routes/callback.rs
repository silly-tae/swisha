//! `POST /swish/callback`, the public listener's only route.

use std::{collections::HashSet, net::SocketAddr, sync::OnceLock};

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;

use crate::events::{log_swish_event, publish_swish_update, SwishLogFields};
use crate::store::PayoutStore;
use crate::{
    http::client_ip::client_ip,
    state::SharedState,
};

#[derive(Debug, Deserialize)]
/// What Swish posts back when a payout resolves.
///
/// Every field is optional because the body is whatever Swish sends, not whatever swisha hopes
/// for. A missing reference is dropped rather than guessed at.
pub struct CallbackBody {
    /// The payout's new status.
    pub status:                                     Option<String>,
    #[serde(rename = "payerPaymentReference")]
    /// The reference swisha originally supplied.
    pub payer_payment_reference:                    Option<String>,
    #[serde(rename = "payoutInstructionUUID")]
    /// The instruction this concerns. Checked against the stored one, so a forged callback
    /// cannot settle a payout without guessing it.
    pub payout_instruction_uuid:                    Option<String>,
    #[serde(rename = "errorCode")]
    /// Swish's error code, on failures.
    pub error_code:                                 Option<String>,
    #[serde(rename = "errorMessage")]
    /// Swish's error text, on failures.
    pub error_message:                              Option<String>,
}

static SWISH_IPS: OnceLock<HashSet<&'static str>> = OnceLock::new();

fn swish_ips() -> &'static HashSet<&'static str> {
    SWISH_IPS.get_or_init(|| {
        HashSet::from([
            "89.46.83.171",
            "213.132.115.90",
            "213.132.115.86",
            "193.53.81.202",
            "193.53.81.203",
            "193.53.81.204",
            "193.53.81.205",
            "213.132.115.94",
        ])
    })
}

/// Swish's published callback addresses, sorted. Enforced only when `SWISH_ENV=production`.
pub fn active_callback_ips() -> Vec<&'static str> {
    let mut ips: Vec<&'static str> = swish_ips().iter().copied().collect();
    ips.sort();
    ips
}

/// `POST /swish/callback`. Records the outcome Swish reports.
///
/// Refuses anything outside the allowlist in production, ignores a callback whose instruction
/// UUID does not match the stored one, and never overwrites a terminal status.
pub async fn callback(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<CallbackBody>,
) -> StatusCode {
    let ip = client_ip(&headers, addr, &state.config.trusted_proxies).to_string();

    if state.config.swish_env == "production" {
        if !swish_ips().contains(ip.as_str()) {
            tracing::warn!("[swish/callback] rejected request from unknown IP: {ip}");
            return StatusCode::FORBIDDEN;
        }
    } else if !swish_ips().contains(ip.as_str()) {
        tracing::warn!("[swish/callback] non-production: accepting callback from non-allowlisted IP: {ip}");
    }

    let reference = match body.payer_payment_reference.as_deref().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => {
            tracing::warn!("[swish/callback] received callback with missing payer_payment_reference from {ip}");
            return StatusCode::OK;
        }
    };
    let callback_ref = body.payout_instruction_uuid.clone().unwrap_or_default();
    let status = match body.status.as_deref() {
        Some(s @ ("PAID" | "DEBITED" | "DECLINED" | "ERROR" | "PENDING")) => s.to_string(),
        Some(other) => {
            tracing::warn!("[swish/callback] unexpected status '{other}' for {reference}, storing as-is");
            other.to_string()
        }
        None => "UNKNOWN".to_string(),
    };

    let stored = match state.store.snapshot(&reference).await {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!("[swish/callback] store error fetching payout for {reference}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let stored_status = match stored {
        None => {
            tracing::warn!("[swish/callback] no payout for reference: {reference}");
            return StatusCode::OK;
        }
        Some(snapshot) if snapshot.swish_ref.as_deref() != Some(&callback_ref) => {
            tracing::warn!(
                "[swish/callback] UUID mismatch – reference: {reference}, \
                 received: {callback_ref}, stored: {}",
                snapshot.swish_ref.as_deref().unwrap_or("none")
            );
            return StatusCode::OK;
        }
        Some(snapshot) => snapshot.status,
    };

    // TERMINAL statuses are never overwritten, even by a delayed or retried callback.
    let applied = state
        .store
        .set_status_unless_terminal(&reference, &status)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("[swish/callback] failed to update status for {reference}: {e}");
            false
        });

    // pg_notify has no replay, so a client that misses one notification never recovers from it.
    // The callback therefore always announces, even when the terminal guard blocked the write
    // and the poll already announced once. It announces the stored status rather than the
    // callback's, so a late or out-of-order callback cannot walk a settled payout backwards.
    let effective_status = if applied {
        status.clone()
    } else {
        stored_status.unwrap_or_else(|| status.clone())
    };
    let effective_error = if applied { body.error_code.clone() } else { None };

    {
        let s    = state.clone();
        let kvnr = reference.clone();
        let cr   = callback_ref.clone();
        tokio::spawn(async move {
            publish_swish_update(&kvnr, &effective_status, Some(&cr), effective_error.as_deref(), &s).await;
        });
    }

    if applied && status == "DEBITED" {
        let s    = state.clone();
        let kvnr = reference.clone();
        let cr   = callback_ref.clone();
        tokio::spawn(async move {
            log_swish_event(
                "PAID",
                SwishLogFields {
                    reference:    kvnr,
                    swish_ref:     Some(cr),
                    status:        Some("DEBITED".into()),
                    amount:        None,
                    payee_alias: None,
                    error_code:    None,
                    error_message: None,
                    ip:            None,
                },
                &s,
            )
            .await;
        });
    }

    {
        let s = state.clone();
        tokio::spawn(async move {
            log_swish_event(
                "CALLBACK",
                SwishLogFields {
                    reference,
                    swish_ref:     Some(callback_ref),
                    status:        Some(status),
                    amount:        None,
                    payee_alias: None,
                    error_code:    body.error_code,
                    error_message: body.error_message,
                    ip:            Some(ip),
                },
                &s,
            )
            .await;
        });
    }

    StatusCode::OK
}

//! The crate's only submission to Swish, and polling for the outcome.

use serde::Serialize;
use serde_json::{Value, value::RawValue};
use tokio::time::{sleep, timeout, Duration};

use crate::error::ApiError;
use crate::store::PayoutStore;
use crate::{
    state::SharedState,
    swish::{
        client::{swish_get, swish_post},
        payload::{ExecutePayoutArgs, PayoutPayload},
        sign::sign_payload,
    },
};

// The only function in swisha that POSTs a payout instruction to Swish. Returns Ok(()) on 201.
// The caller polls separately, so the request can be answered before Swish resolves.
/// Submits a payout to Swish.
///
/// **This is the crate's only `POST` to Swish, and it has exactly one call site.**
/// `tests/no_resubmission.rs` asserts both by scanning the source. Adding a second call site is
/// how a payout gets made twice.
pub async fn submit_payout(args: &ExecutePayoutArgs<'_>, state: &SharedState) -> Result<(), ApiError> {
    let payload = PayoutPayload::new(
        args,
        &state.config.swish_number,
        &state.config.swish_signing_serial,
        crate::util::time::now_utc().utc_z(),
    );

    let payload_str = serde_json::to_string(&payload)
        .map_err(|e| ApiError::Internal(e.into()))?;

    let signature = sign_payload(&payload_str, &state.signing_key)
        .map_err(|_| ApiError::Internal(crate::error::err("Failed to sign payout.")))?;

    let raw_payload = RawValue::from_string(payload_str)
        .map_err(|e| ApiError::Internal(e.into()))?;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct MssPayoutRequest<'a> {
        payload:      &'a RawValue,
        callback_url: &'a str,
        signature:    &'a str,
    }

    let request_body = MssPayoutRequest {
        payload:      &raw_payload,
        callback_url: &state.config.swish_callback_url,
        signature:    &signature,
    };

    let mss_url = format!("{}/swish-cpcapi/api/v1/payouts", state.config.swish_base_url);

    // Not reaching Swish is Swish being unavailable, not swisha failing. Reported as such so a
    // caller can tell "try later" from "something here is broken", and so the reason survives
    // into the answer instead of being swallowed by Internal's deliberate silence.
    let resp = swish_post(&mss_url, &request_body, &state.swish_client)
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("Could not reach Swish: {e}")))?;

    let status_code = resp.status().as_u16();
    if status_code != 201 {
        let body = resp.text().await.unwrap_or_else(|e| {
            tracing::warn!("[swish/execute] failed to read error response body: {e}");
            String::new()
        });
        return Err(ApiError::SwishRejected { code: status_code, body });
    }

    Ok(())
}

// 8 attempts ≈ 48s worst case. Returns Ok("PENDING") when Swish hasn't resolved yet
// (callback will deliver the final status). Returns Err if Swish was unreachable on
// every attempt – caller should treat that as a hard failure, not a pending payment.
/// Polls Swish for an outcome, up to 8 times over roughly 48 seconds.
///
/// Returns `PENDING` when Swish has not resolved the payout in that window; a sweep picks it up
/// later. Reads only: this never resubmits.
pub async fn poll_payout_status(payout_uuid: &str, state: &SharedState) -> Result<String, ApiError> {
    let url = format!(
        "{}/swish-cpcapi/api/v1/payouts/{}",
        state.config.swish_base_url,
        payout_uuid,
    );

    let mut any_contact = false;
    let mut saw_404     = false;

    for attempt in 0u8..8 {
        // Short first delay: Swish usually resolves within a second of the POST.
        let delay = if attempt == 0 { Duration::from_millis(500) } else { Duration::from_secs(2) };
        sleep(delay).await;

        let result = timeout(Duration::from_secs(4), swish_get(&url, &state.swish_client)).await;
        match result {
            Ok(Ok(resp)) if resp.status().is_success() => {
                any_contact = true;
                match resp.json::<Value>().await {
                    Ok(data) => match data["status"].as_str() {
                        Some("PAID") | Some("DEBITED") | Some("DECLINED") | Some("ERROR") => {
                            return Ok(data["status"].as_str().unwrap_or("ERROR").to_string());
                        }
                        _ => {}
                    },
                    Err(e) => {
                        tracing::warn!("[swish/poll] failed to parse response on attempt {}: {e}", attempt + 1);
                    }
                }
            }
            Ok(Ok(resp)) => {
                any_contact = true;
                let code = resp.status().as_u16();
                if code == 404 { saw_404 = true; }
                tracing::warn!("[swish/poll] non-OK on attempt {}: {}", attempt + 1, resp.status());
            }
            _ => {
                tracing::warn!("[swish/poll] attempt {} failed or timed out", attempt + 1);
            }
        }
    }

    if !any_contact {
        return Err(ApiError::ServiceUnavailable(
            "Swish API unreachable during status polling.".into(),
        ));
    }

    // 404 on any poll attempt means Swish has no record of this payout – PENDING would be wrong.
    if saw_404 {
        return Err(ApiError::ServiceUnavailable(
            format!("Swish has no record of payout {payout_uuid} (404).")
        ));
    }

    Ok("PENDING".to_string())
}

/// Writes a status unless the payout already holds a terminal one, and reports whether it
/// applied.
pub async fn update_swish_status(reference: &str, status: &str, state: &SharedState) -> bool {
    match state.store.set_status_unless_terminal(reference, status).await {
        Ok(applied) => applied,
        Err(e) => {
            tracing::warn!("[swish/status] failed to update status to '{status}' for {reference}: {e}");
            false
        }
    }
}

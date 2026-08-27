//! `POST /swish/payout`.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde_json::{json, Value};

use crate::domain::{
    payout::NewPayout,
    request::PayoutRequest,
    validate::{normalize_phone, normalize_ssn, personnummer_luhn_valid, validate_phone},
};
use crate::error::ApiError;
use crate::http::{
    client_ip::{PeerAddr, client_ip},
    rate_limit, secret,
};
use crate::store::PayoutStore;
use crate::events::{SwishLogFields, log_admin_event, log_swish_event, publish_swish_update};
use crate::{
    state::SharedState,
    swish::{
        payload::ExecutePayoutArgs,
        random_payout_uuid,
        submit::{poll_payout_status, submit_payout, update_swish_status},
    },
};

/// `POST /swish/payout`. Validates, claims the reference, submits once, and returns `202`.
///
/// A reference that is already spent returns `409` whatever state the payout reached. That is
/// never a reason to retry: see [`crate::domain::status::FIELDS_LOCKED`].
pub async fn payout(
    State(state): State<SharedState>,
    peer: PeerAddr,
    headers: HeaderMap,
    Json(body): Json<PayoutRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !secret::authorized(&headers, state.config.api_secret.as_deref()) {
        return Err(ApiError::Unauthorized);
    }

    // Over a Unix socket there is no peer address: the connection is already restricted to
    // processes the kernel let through, so every caller shares one rate-limit bucket.
    let caller = match peer.0 {
        Some(addr) => client_ip(&headers, addr, &state.config.trusted_proxies),
        None => std::net::IpAddr::from([127, 0, 0, 1]),
    };
    if !rate_limit::allow(caller) {
        tracing::warn!("[swish/payout] rate limit exceeded for {caller}");
        return Err(ApiError::TooManyRequests);
    }

    let ip = caller.to_string();

    if body.reference.trim().is_empty() {
        return Err(ApiError::BadRequest("reference is required.".into()));
    }
    if body.reference.chars().count() > 35 {
        return Err(ApiError::BadRequest(
            "reference must be at most 35 characters.".into(),
        ));
    }
    if body.reference.chars().any(|c| c.is_control()) {
        return Err(ApiError::BadRequest(
            "reference contains invalid characters.".into(),
        ));
    }

    if !body.amount.is_finite() {
        return Err(ApiError::BadRequest("amount is not a valid number.".into()));
    }
    if body.amount < 1.0 || body.amount > state.config.swish_max_payout {
        return Err(ApiError::BadRequest(format!(
            "amount must be between 1 and {} SEK.",
            state.config.swish_max_payout
        )));
    }

    let payee_alias = normalize_phone(&body.payee_alias);
    if !validate_phone(&payee_alias) {
        return Err(ApiError::BadRequest(format!(
            "invalid Swish number: {}.",
            body.payee_alias
        )));
    }

    // Rejected rather than dropped. A caller that supplied an identity number asked Swish to
    // check it against the phone number, and sending the payout without it is not that check.
    // Blank counts as absent, the same way a blank setting does.
    let supplied = body.payee_ssn.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let payee_ssn: Option<String> = match supplied {
        None if state.config.require_ssn => {
            return Err(ApiError::BadRequest(
                "payee_ssn is required: this instance verifies every payout against the \
                 recipient's personnummer.".into(),
            ));
        }
        None => None,
        Some(raw) => {
            let ssn = normalize_ssn(raw);
            if ssn.len() != 12 || !personnummer_luhn_valid(&ssn) {
                return Err(ApiError::BadRequest(
                    "payee_ssn must be a 12-digit personnummer (YYYYMMDDNNNN).".into(),
                ));
            }
            Some(ssn)
        }
    };

    // Swish shows this to the recipient and truncates past 50 characters.
    let message = match body.message.as_deref() {
        Some(text) if !text.trim().is_empty() => text.to_string(),
        _ => state.config.payout_message.replace("{reference}", &body.reference),
    };
    if message.chars().any(|c| c.is_control()) {
        return Err(ApiError::BadRequest(
            "message contains invalid characters.".into(),
        ));
    }

    let payout_uuid = random_payout_uuid();

    let claim = state
        .store
        .claim(&NewPayout {
            reference:   &body.reference,
            payee_alias: &payee_alias,
            payee_ssn:   payee_ssn.as_deref(),
            amount:      body.amount,
            message:     &message,
            swish_ref:   &payout_uuid,
        })
        .await
        .map_err(|e| {
            tracing::warn!("Failed to persist payout record: {e}");
            ApiError::ServiceUnavailable("Failed to persist payout record.".into())
        })?;

    let (upserted_ref, upserted_status) = (claim.swish_ref.clone(), claim.status.clone());

    match upserted_status.as_str() {
        "PAID" => {
            return Err(ApiError::Conflict(format!(
                "Payout already completed for this reference. Swish ref: {}",
                upserted_ref.as_deref().unwrap_or("")
            )));
        }
        "PENDING" | "DEBITED" => {
            return Err(ApiError::Conflict(format!(
                "Payout already in progress for this reference. Swish ref: {}",
                upserted_ref.as_deref().unwrap_or("")
            )));
        }
        "NEEDS_REVIEW" => {
            return Err(ApiError::Conflict(
                "swisha could not resolve this payout. Check it with Swish, then submit it under a new reference if it should still be paid.".into(),
            ));
        }
        // A failed payout is not a free one. Swish may have received the original and only the
        // answer went missing, so resubmitting under a new UUID can debit a second time.
        "ERROR" => {
            return Err(ApiError::Conflict(
                "This payout failed, but Swish may still have received it. Check before paying again, and use a new reference if it should still go out.".into(),
            ));
        }
        "DECLINED" => {
            return Err(ApiError::Conflict(
                "Swish declined this payout. Submit it under a new reference if it should still be paid.".into(),
            ));
        }
        "CREATED" if upserted_ref.as_deref() != Some(payout_uuid.as_str()) => {
            return Err(ApiError::Conflict(
                "A payout for this reference is already in progress.".into(),
            ));
        }
        _ => {}
    }

    // Fire-and-forget admin + INITIATED logs in parallel.
    {
        let s    = state.clone();
        let kvnr = body.reference.clone();
        let pref = payout_uuid.clone();
        let amt  = body.amount;
        let tel  = payee_alias.clone();
        let ip_  = ip.clone();
        let msg  = format!("Payout {kvnr} accepted");
        tokio::spawn(async move {
            tokio::join!(
                log_admin_event(
                    "info", &msg,
                    serde_json::json!({ "reference": &kvnr, "amount": amt }),
                    &ip_, &s,
                ),
                log_swish_event(
                    "INITIATED",
                    SwishLogFields {
                        reference: kvnr, swish_ref: Some(pref),
                        status: None, amount: Some(amt), payee_alias:   Some(tel),
                        error_code: None, error_message: None, ip: Some(ip_.clone()),
                    },
                    &s,
                ),
            );
        });
    }

    let args = ExecutePayoutArgs {
        reference:     &body.reference,
        payout_uuid:   &payout_uuid,
        payee_alias:   &payee_alias,
        payee_ssn:     payee_ssn.as_deref(),
        amount:        body.amount,
        message:       &message,
    };

    // Submit to Swish synchronously – only the POST. Return 202 immediately on acceptance;
    // poll runs in the background and delivers the final status via SSE.
    match submit_payout(&args, &state).await {
        Err(ApiError::SwishRejected { code, body: err_body }) => {
            update_swish_status(&body.reference, "ERROR", &state).await;
            {
                let s    = state.clone();
                let kvnr = body.reference.clone();
                let pref = payout_uuid.clone();
                let code_str = code.to_string();
                let err  = err_body.clone();
                let ip_  = ip.clone();
                tokio::spawn(async move {
                    let ec = code_str.clone();
                    tokio::join!(
                        log_swish_event(
                            "ERROR",
                            SwishLogFields {
                                reference: kvnr.clone(), swish_ref: Some(pref.clone()),
                                status: None, amount: None, payee_alias:   None,
                                error_code: Some(code_str), error_message: Some(err),
                                ip: Some(ip_),
                            },
                            &s,
                        ),
                        publish_swish_update(&kvnr, "ERROR", Some(&pref), Some(ec.as_str()), &s),
                    );
                });
            }
            Err(ApiError::ServiceUnavailable(format!(
                "Swish rejected the payout ({code}): {err_body}"
            )))
        }
        Err(e) => {
            update_swish_status(&body.reference, "ERROR", &state).await;
            {
                let s    = state.clone();
                let kvnr = body.reference.clone();
                let pref = payout_uuid.clone();
                let err  = e.to_string();
                let ip_  = ip.clone();
                tokio::spawn(async move {
                    tokio::join!(
                        log_swish_event(
                            "ERROR",
                            SwishLogFields {
                                reference: kvnr.clone(), swish_ref: Some(pref.clone()),
                                status: None, amount: None, payee_alias:   None,
                                error_code: None, error_message: Some(err),
                                ip: Some(ip_),
                            },
                            &s,
                        ),
                        publish_swish_update(&kvnr, "ERROR", Some(&pref), None, &s),
                    );
                });
            }
            Err(e)
        }
        Ok(()) => {
            let s    = state.clone();
            let kvnr = body.reference.clone();
            let pref = payout_uuid.clone();
            let amt  = body.amount;
            let tel  = payee_alias.clone();
            let ip_  = ip.clone();
            tokio::spawn(async move {
                let final_status = match poll_payout_status(&pref, &s).await {
                    Ok(st) => st,
                    Err(e) => {
                        tracing::warn!("[swish/payout] poll failed for {kvnr}: {e}");
                        "ERROR".to_string()
                    }
                };
                let updated = update_swish_status(&kvnr, &final_status, &s).await;
                log_swish_event(
                    &final_status,
                    SwishLogFields {
                        reference:     kvnr.clone(),
                        swish_ref:     Some(pref.clone()),
                        status:        Some(final_status.clone()),
                        amount:        Some(amt),
                        payee_alias:   Some(tel),
                        error_code:    None,
                        error_message: None,
                        ip:            Some(ip_.clone()),
                    },
                    &s,
                ).await;
                if final_status == "DEBITED" && updated {
                    log_swish_event(
                        "PAID",
                        SwishLogFields {
                            reference:     kvnr.clone(),
                            swish_ref:     Some(pref.clone()),
                            status:        Some("DEBITED".into()),
                            amount:        Some(amt),
                            payee_alias:   None,
                            error_code:    None,
                            error_message: None,
                            ip:            Some(ip_),
                        },
                        &s,
                    ).await;
                }
                publish_swish_update(&kvnr, &final_status, Some(&pref), None, &s).await;
            });

            Ok((StatusCode::ACCEPTED, Json(json!({ "success": true, "swish_ref": payout_uuid, "status": "CREATED" }))))
        }
    }
}

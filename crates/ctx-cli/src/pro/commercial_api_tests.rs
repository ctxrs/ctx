use super::referral::{
    validate_payout_identity, validate_referral_claim_token, ReferralCreateRequest,
    ReferralPayoutRequest, MAX_REFERRAL_CENTS, MAX_REFERRAL_CENTS_PER_ATTRIBUTION, REFERRALS_PATH,
    REFERRAL_PAYOUT_PATH,
};
use super::*;
use ctx_pro_host_protocol::{EntitlementAccessKind, EntitlementGrant, ENTITLEMENT_SCHEMA_VERSION};

fn test_client() -> CommercialApiClient {
    CommercialApiClient::new(CommercialApiConfig {
        origin: Url::parse("https://pro.ctx.test/").unwrap(),
    })
    .unwrap()
}

fn test_response(
    status: u16,
    content_type: &str,
    retry_after: Option<&str>,
    body: &str,
) -> ureq::Response {
    let retry_after = retry_after
        .map(|value| format!("Retry-After: {value}\r\n"))
        .unwrap_or_default();
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\n{retry_after}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .parse()
    .unwrap()
}

#[test]
fn access_state_is_explicit() {
    let parse = |state: &str, deadline| CommercialState {
        subject: "user_123".to_owned(),
        account_id: "org_123".to_owned(),
        access_state: state.to_owned(),
        access_deadline_unix: deadline,
        billing: BillingState {
            customer_associated: true,
            subscription_status: None,
            trial_end_unix: None,
            current_period_end_unix: None,
            cancel_at_period_end: false,
            canceled_at_unix: None,
            latest_invoice_status: None,
            latest_payment_state: "unknown".to_owned(),
        },
    };
    assert!(parse("trial", Some(100)).grants_access());
    assert!(!parse("locked", Some(100)).grants_access());
    assert!(!parse("active", None).grants_access());
    parse("trial", Some(100)).validate().unwrap();
    parse("locked", Some(100)).validate().unwrap();
    parse("locked", None).validate().unwrap();
    parse("none", None).validate().unwrap();
    assert!(parse("none", Some(100)).validate().is_err());
    assert!(parse("active", None).validate().is_err());
}

#[test]
fn accepts_url_less_pending_checkout_from_worker() {
    let now = unix_time().unwrap();
    let pending: CheckoutResult = serde_json::from_value(serde_json::json!({
        "kind": "checkout_pending",
        "expires_at_unix": now + 300,
    }))
    .unwrap();
    pending.validate().unwrap();

    let invalid: CheckoutResult = serde_json::from_value(serde_json::json!({
        "kind": "checkout_pending",
        "url": "https://checkout.stripe.com/session",
        "expires_at_unix": now + 300,
    }))
    .unwrap();
    assert!(invalid.validate().is_err());
}

#[test]
fn pins_created_checkout_and_portal_urls_to_exact_stripe_hosts() {
    let now = unix_time().unwrap();
    for invalid in [
        "https://stripe.example.test/session",
        "https://checkout.stripe.com.evil.test/session",
        "https://checkout.stripe.com:444/session",
    ] {
        let checkout: CheckoutResult = serde_json::from_value(serde_json::json!({
            "kind": "checkout_created",
            "url": invalid,
            "expires_at_unix": now + 300,
        }))
        .unwrap();
        assert!(checkout.validate().is_err(), "{invalid}");
    }
    let checkout: CheckoutResult = serde_json::from_value(serde_json::json!({
        "kind": "checkout_created",
        "url": "https://checkout.stripe.com/session",
        "expires_at_unix": now + 300,
    }))
    .unwrap();
    checkout.validate().unwrap();

    assert!(urls::validate_portal_url("https://billing.stripe.com/session").is_ok());
    assert!(urls::validate_portal_url("https://billing.stripe.com.evil.test/session").is_err());
}

#[test]
fn rejects_inconsistent_unassociated_billing() {
    let state = CommercialState {
        subject: "user_123".to_owned(),
        account_id: "org_123".to_owned(),
        access_state: "none".to_owned(),
        access_deadline_unix: None,
        billing: BillingState {
            customer_associated: false,
            subscription_status: Some("active".to_owned()),
            trial_end_unix: None,
            current_period_end_unix: None,
            cancel_at_period_end: false,
            canceled_at_unix: None,
            latest_invoice_status: None,
            latest_payment_state: "unknown".to_owned(),
        },
    };
    assert!(state.validate().is_err());
}

#[test]
fn rejects_non_https_urls() {
    assert!(validate_https_url("http://billing.example.test", "portal").is_err());
    assert!(validate_https_url("https://user@billing.example.test", "portal").is_err());
    validate_https_url("https://billing.example.test/session", "portal").unwrap();
}

#[test]
fn commercial_request_bodies_cannot_carry_history_content() {
    assert_eq!(
        serde_json::to_value(EmptyRequest {}).unwrap(),
        serde_json::json!({})
    );
    assert_eq!(
        serde_json::to_value(EntitlementRequest {
            installation_public_key_base64url: "installation-key",
        })
        .unwrap(),
        serde_json::json!({
            "installation_public_key_base64url": "installation-key",
        })
    );
    assert_eq!(
        serde_json::to_value(CheckoutRequest {
            referral_claim_token: None,
        })
        .unwrap(),
        serde_json::json!({})
    );
    assert_eq!(
        serde_json::to_value(CheckoutRequest {
            referral_claim_token: Some("claim.opaque_123456"),
        })
        .unwrap(),
        serde_json::json!({
            "referral_claim_token": "claim.opaque_123456",
        })
    );
}

#[test]
fn trial_requests_send_raw_codename_only_on_the_first_challenge() {
    let challenge = |referral_codename| TrialChallengeRequest {
        schema_version: 1,
        channel: "staging",
        target: "x86_64-unknown-linux-gnu",
        current_version: Some("0.26.0"),
        protocol_version: 1,
        protocol_fingerprint: "fingerprint",
        installation_public_key_base64url: "installation-key",
        referral_codename,
    };
    let ordinary = serde_json::to_value(challenge(None)).unwrap();
    assert_eq!(
        ordinary,
        serde_json::json!({
            "schema_version": 1,
            "channel": "staging",
            "target": "x86_64-unknown-linux-gnu",
            "current_version": "0.26.0",
            "protocol_version": 1,
            "protocol_fingerprint": "fingerprint",
            "installation_public_key_base64url": "installation-key",
        })
    );
    let referred = serde_json::to_value(challenge(Some("agent-smith"))).unwrap();
    assert_eq!(referred["referral_codename"], "agent-smith");

    let activation = serde_json::to_value(TrialActivationRequest {
        schema_version: 1,
        challenge_id: "challenge-123",
        installation_public_key_base64url: "installation-key",
        evidence: &serde_json::json!({"opaque": true}),
    })
    .unwrap();
    let refresh = serde_json::to_value(TrialRefreshRequest {
        schema_version: 1,
        installation_public_key_base64url: "installation-key",
    })
    .unwrap();
    for body in [activation, refresh] {
        assert!(body.get("referral_codename").is_none());
        assert!(body.get("referral_claim_token").is_none());
    }
}

#[test]
fn referral_and_payout_request_bodies_are_minimal_and_bounded() {
    assert_eq!(REFERRALS_PATH, "/v1/referrals");
    assert_eq!(REFERRAL_PAYOUT_PATH, "/v1/referrals/payout");
    assert_eq!(
        serde_json::to_value(ReferralCreateRequest {
            codename: "agent-smith"
        })
        .unwrap(),
        serde_json::json!({"codename": "agent-smith"})
    );
    assert_eq!(
        serde_json::to_value(ReferralPayoutRequest {
            country: None,
            entity_type: None,
        })
        .unwrap(),
        serde_json::json!({})
    );
    assert_eq!(
        serde_json::to_value(ReferralPayoutRequest {
            country: Some("US"),
            entity_type: Some("individual"),
        })
        .unwrap(),
        serde_json::json!({
            "country": "US",
            "entity_type": "individual",
        })
    );
    validate_payout_identity(Some("US"), Some("company")).unwrap();
    for invalid in [
        (Some("us"), None),
        (Some("USA"), None),
        (None, Some("person")),
    ] {
        assert!(validate_payout_identity(invalid.0, invalid.1).is_err());
    }
}

#[test]
fn commercial_requests_accept_success_and_error_media_types() {
    assert_eq!(
        COMMERCIAL_ACCEPT,
        "application/json, application/problem+json"
    );
}

#[test]
fn commercial_conflicts_have_bounded_recovery_actions() {
    for (code, expected) in [
        (
            "billing_conflict",
            "multiple active subscriptions need attention; run `ctx pro manage` to resolve them",
        ),
        (
            "commercial_identity_conflict",
            "the billing customer belongs to a different signed-in account; rerun `ctx pro` with the original account",
        ),
    ] {
        let error = ApiError {
            code: code.to_owned(),
            message: "bounded upstream detail".to_owned(),
            retryable: false,
        };
        assert_eq!(commercial_error_message(&error), expected);
    }
}

#[test]
fn referral_failures_render_only_stable_safe_copy() {
    let secret = "private-codename-and-upstream-detail";
    for (code, public_code, expected) in [
        (
            "payout_setup_unavailable",
            "referral_payout_unavailable",
            "payout onboarding is not currently available",
        ),
        (
            "referral_claim_conflict",
            "commercial_identity_conflict",
            "attribution conflicts with existing account state",
        ),
        (
            "referral_claim_invalid",
            "invalid_request",
            "attribution is invalid",
        ),
        (
            "referral_codename_immutable",
            "referral_codename_conflict",
            "already has a different referral codename",
        ),
        (
            "referral_codename_invalid",
            "invalid_request",
            "codename is invalid",
        ),
        (
            "referral_codename_not_found",
            "referral_not_found",
            "codename was not found",
        ),
        (
            "referral_codename_reserved",
            "invalid_request",
            "codename is reserved",
        ),
        (
            "referral_codename_taken",
            "referral_codename_conflict",
            "codename is already claimed",
        ),
        (
            "referral_payout_not_eligible",
            "referral_not_eligible",
            "a payable referral balance is required",
        ),
        (
            "referral_self_referral",
            "referral_self_referral",
            "self-referrals are not eligible",
        ),
        (
            "referral_verified_email_required",
            "authentication_required",
            "a verified WorkOS email is required",
        ),
    ] {
        let failure = typed_api_failure(
            503,
            None,
            ApiFailure {
                api_version: "v1".to_owned(),
                request_id: "request_123".to_owned(),
                error: ApiError {
                    code: code.to_owned(),
                    message: secret.to_owned(),
                    retryable: true,
                },
            },
        )
        .unwrap();
        assert!(!failure.is_retryable(), "{code}");
        let rendered = anyhow!(failure).to_string();
        assert!(rendered.starts_with(public_code), "{code}: {rendered}");
        assert!(rendered.contains(expected), "{code}: {rendered}");
        assert!(!rendered.contains(secret), "{code}: {rendered}");
    }
}

#[test]
fn unknown_hosted_error_codes_are_always_safe_invalid_responses() {
    let unknown_codename = "privatecodename";
    for status in [400, 503] {
        let failure = typed_api_failure(
            status,
            Some(Duration::from_secs(60)),
            ApiFailure {
                api_version: "v1".to_owned(),
                request_id: "request_123".to_owned(),
                error: ApiError {
                    code: unknown_codename.to_owned(),
                    message: "private hosted detail".to_owned(),
                    retryable: true,
                },
            },
        )
        .unwrap();
        assert!(matches!(
            &failure,
            CommercialApiFailure::InvalidResponse {
                status: actual_status
            } if *actual_status == status
        ));
        let rendered = failure.to_string();
        assert!(rendered.starts_with("invalid_response:"));
        assert!(!rendered.contains(unknown_codename));
        assert!(!rendered.contains("private hosted detail"));
        assert!(!failure.is_retryable());
        assert_eq!(failure.retry_after(), None);
    }
}

#[test]
fn checkout_poll_retries_only_transient_commercial_failures() {
    let response = |code: &str, status, retryable| {
        anyhow!(CommercialApiFailure::Response {
            code: code.to_owned(),
            message: "bounded failure",
            status,
            retryable,
            retry_after: None,
        })
    };
    for error in [
        anyhow!(CommercialApiFailure::Transport {
            operation: "commercial API request".to_owned(),
        }),
        anyhow!(CommercialApiFailure::Proxy {
            status: 503,
            retry_after: None,
        }),
        response("rate_limited", 429, true),
        response("service_unavailable", 503, true),
        response("dependency_unavailable", 503, true),
        response("upstream_failure", 500, true),
        anyhow!("service_unavailable: read commercial API response"),
    ] {
        assert!(is_retryable_checkout_failure(&error), "{error:#}");
    }
    for error in [
        response("authentication_required", 503, true),
        response("billing_conflict", 503, true),
        response("commercial_identity_conflict", 503, true),
        response("commercial_access_locked", 402, false),
        response("dependency_invalid_response", 502, true),
        response("rate_limited", 429, false),
        response("upstream_failure", 500, false),
        anyhow!(CommercialApiFailure::InvalidResponse { status: 400 }),
        anyhow!("invalid_response: malformed commercial account"),
    ] {
        assert!(!is_retryable_checkout_failure(&error), "{error:#}");
    }
}

#[test]
fn checkout_retry_after_accepts_bounded_delta_seconds() {
    assert_eq!(parse_retry_after(Some("60")), Some(Duration::from_secs(60)));
    assert_eq!(
        parse_retry_after(Some("999999")),
        Some(Duration::from_secs(MAX_RETRY_AFTER_SECONDS))
    );
    assert_eq!(parse_retry_after(Some("not-a-delay")), None);
    assert_eq!(parse_retry_after(None), None);

    let error = anyhow!(CommercialApiFailure::Response {
        code: "rate_limited".to_owned(),
        message: "bounded failure",
        status: 429,
        retryable: true,
        retry_after: Some(Duration::from_secs(60)),
    });
    assert_eq!(checkout_retry_after(&error), Some(Duration::from_secs(60)));
}

#[test]
fn exact_worker_problem_envelopes_preserve_typed_retry_contract() {
    let worker_error = |status, code: &str, retryable, retry_after| {
        assert_eq!(
            classify_error_response(status, Some("application/problem+json; charset=utf-8")),
            ErrorResponseKind::Contracted
        );
        let failure: ApiFailure = serde_json::from_value(serde_json::json!({
            "api_version": "v1",
            "request_id": "123e4567-e89b-12d3-a456-426614174000",
            "error": {
                "code": code,
                "message": "bounded Worker detail",
                "retryable": retryable,
            },
        }))
        .unwrap();
        anyhow!(typed_api_failure(status, retry_after, failure).unwrap())
    };

    for error in [
        worker_error(401, "authentication_required", false, None),
        worker_error(409, "billing_conflict", false, None),
        worker_error(502, "dependency_invalid_response", false, None),
    ] {
        assert!(!is_retryable_checkout_failure(&error), "{error:#}");
        assert_eq!(checkout_retry_after(&error), None);
    }

    let rate_limited = worker_error(429, "rate_limited", true, Some(Duration::from_secs(60)));
    assert!(is_retryable_checkout_failure(&rate_limited));
    assert_eq!(
        checkout_retry_after(&rate_limited),
        Some(Duration::from_secs(60))
    );

    let unavailable = worker_error(503, "dependency_unavailable", true, None);
    assert!(is_retryable_checkout_failure(&unavailable));
}

#[test]
fn error_media_types_distinguish_worker_errors_from_proxy_failures() {
    for content_type in [
        "application/problem+json",
        "Application/Problem+Json; charset=utf-8",
        "application/json; charset=utf-8",
    ] {
        assert_eq!(
            classify_error_response(502, Some(content_type)),
            ErrorResponseKind::Contracted
        );
    }
    for status in [429, 502, 503, 504] {
        assert_eq!(
            classify_error_response(status, Some("text/html")),
            ErrorResponseKind::TransientProxy
        );
        let error = anyhow!(CommercialApiFailure::Proxy {
            status,
            retry_after: parse_retry_after(Some("999999")),
        });
        assert!(is_retryable_checkout_failure(&error));
        assert_eq!(
            checkout_retry_after(&error),
            Some(Duration::from_secs(MAX_RETRY_AFTER_SECONDS))
        );
    }
    for status in [400, 401, 403, 404, 409, 422] {
        assert_eq!(
            classify_error_response(status, Some("text/html")),
            ErrorResponseKind::Invalid
        );
    }
}

#[test]
fn success_media_type_remains_strict() {
    assert!(media_type_is(
        Some("application/json; charset=utf-8"),
        "application/json"
    ));
    assert!(!media_type_is(
        Some("application/problem+json; charset=utf-8"),
        "application/json"
    ));
    assert!(!media_type_is(Some("text/html"), "application/json"));
    assert!(!media_type_is(None, "application/json"));
}

#[test]
fn malformed_contracted_errors_fall_back_by_status_without_body_detail() {
    let malformed = |status, body: &[u8]| {
        assert_eq!(
            classify_error_response(status, Some("application/problem+json")),
            ErrorResponseKind::Contracted
        );
        serde_json::from_slice::<ApiFailure>(body)
            .context("invalid_response: parse commercial API error")
            .and_then(|failure| typed_api_failure(status, None, failure))
            .unwrap_or_else(|_| malformed_error_failure(status, None))
    };

    let transient = anyhow!(malformed(503, br#"not-json-secret-upstream-body"#));
    assert!(is_retryable_checkout_failure(&transient));
    assert!(!transient.to_string().contains("secret-upstream-body"));

    let fatal = anyhow!(malformed(
        400,
        br#"{"api_version":"wrong","request_id":"bad","error":{"code":"invalid_request","message":"secret-upstream-body","retryable":false}}"#,
    ));
    assert!(!is_retryable_checkout_failure(&fatal));
    assert!(!fatal.to_string().contains("secret-upstream-body"));
    assert!(fatal.to_string().starts_with("invalid_response:"));
}

#[test]
fn finish_consumes_worker_problem_json_and_sanitizes_proxy_responses() {
    let worker_body = serde_json::json!({
        "api_version": "v1",
        "request_id": "123e4567-e89b-12d3-a456-426614174000",
        "error": {
            "code": "rate_limited",
            "message": "Worker detail is not rendered",
            "retryable": true,
        },
    })
    .to_string();
    let error = test_client()
        .finish::<CommercialState>(
            Err(ureq::Error::Status(
                429,
                test_response(
                    429,
                    "application/problem+json; charset=utf-8",
                    Some("60"),
                    &worker_body,
                ),
            )),
            "commercial API request",
        )
        .unwrap_err();
    assert!(is_retryable_checkout_failure(&error));
    assert_eq!(checkout_retry_after(&error), Some(Duration::from_secs(60)));
    assert!(error.to_string().starts_with("rate_limited:"));
    assert!(!error.to_string().contains("Worker detail"));

    let proxy = test_client()
        .finish::<CommercialState>(
            Err(ureq::Error::Status(
                503,
                test_response(503, "text/html", Some("999999"), "secret proxy body"),
            )),
            "commercial API request",
        )
        .unwrap_err();
    assert!(is_retryable_checkout_failure(&proxy));
    assert_eq!(
        checkout_retry_after(&proxy),
        Some(Duration::from_secs(MAX_RETRY_AFTER_SECONDS))
    );
    assert!(!proxy.to_string().contains("secret proxy body"));

    let wrong_success_media = test_client()
        .finish::<CommercialState>(
            Ok(test_response(
                200,
                "application/problem+json",
                None,
                &worker_body,
            )),
            "commercial API request",
        )
        .unwrap_err();
    assert!(wrong_success_media
        .to_string()
        .starts_with("invalid_response:"));

    let malformed_success = test_client()
        .finish::<CommercialState>(
            Ok(test_response(
                200,
                "application/json",
                None,
                r#"{"secret-success-field":"secret-success-body"}"#,
            )),
            "commercial API request",
        )
        .unwrap_err();
    assert!(malformed_success
        .to_string()
        .starts_with("invalid_response:"));
    assert!(!malformed_success
        .to_string()
        .contains("secret-success-body"));
}

#[test]
fn anonymous_trial_credentials_and_challenges_are_strictly_bounded() {
    assert_eq!(
        trial_authorization("abcDEF_123-456.xyz~").unwrap().as_str(),
        "CtxTrial abcDEF_123-456.xyz~"
    );
    for invalid in ["short", "contains space token", "line\nbreak"] {
        let error = trial_authorization(invalid).unwrap_err().to_string();
        assert!(!error.contains(invalid));
    }
    validate_trial_token(&"a".repeat(MAX_TRIAL_ACCESS_TOKEN_BYTES)).unwrap();
    let oversized = "z".repeat(MAX_TRIAL_ACCESS_TOKEN_BYTES + 1);
    let error = validate_trial_token(&oversized).unwrap_err();
    assert!(error.to_string().starts_with("invalid_response:"));
    assert!(!error.to_string().contains(&oversized));

    let mut challenge = TrialChallenge {
        challenge_id: "challenge-123".to_owned(),
        challenge_base64url: "a".repeat(43),
        expires_at_unix: unix_time().unwrap() + 60,
        artifact_access_token: "a".repeat(32),
    };
    challenge.validate().unwrap();

    challenge.expires_at_unix = unix_time().unwrap() - 1;
    assert!(challenge.validate().is_err());
}

#[test]
fn anonymous_trial_response_debug_redacts_credentials() {
    let challenge_token = "challenge-token-must-not-appear";
    let activation_token = "activation-token-must-not-appear";
    let refresh_token = "refresh-token-must-not-appear";
    let entitlement = trial_entitlement();
    let responses = [
        (
            format!(
                "{:?}",
                TrialChallenge {
                    challenge_id: "challenge-123".to_owned(),
                    challenge_base64url: "a".repeat(43),
                    expires_at_unix: unix_time().unwrap() + 60,
                    artifact_access_token: challenge_token.to_owned(),
                }
            ),
            challenge_token,
        ),
        (
            format!(
                "{:?}",
                TrialActivation {
                    disposition: "trial_started".to_owned(),
                    entitlement: entitlement.clone(),
                    trial_access_token: activation_token.to_owned(),
                    trial_deadline_unix: 2_000_000_000,
                    referral_claim_token: None,
                }
            ),
            activation_token,
        ),
        (
            format!(
                "{:?}",
                TrialRefresh {
                    entitlement,
                    trial_access_token: refresh_token.to_owned(),
                    trial_deadline_unix: 2_000_000_000,
                    referral_claim_token: None,
                }
            ),
            refresh_token,
        ),
    ];

    for (debug, token) in responses {
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(!debug.contains(token), "{debug}");
        assert!(!debug.contains("entitlement-signature-canary"), "{debug}");
    }
}

#[test]
fn anonymous_trial_plaintext_copies_are_drop_guarded() {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    fn assert_value_zeroizes_on_drop<T: zeroize::ZeroizeOnDrop>(_: &T) {}

    assert_zeroize_on_drop::<TrialChallenge>();
    assert_zeroize_on_drop::<TrialActivation>();
    assert_zeroize_on_drop::<TrialRefresh>();

    let bearer = bearer_authorization("commercial-token-contract-canary").unwrap();
    let trial = trial_authorization("trial-token-contract-canary").unwrap();
    assert_value_zeroizes_on_drop(&bearer);
    assert_value_zeroizes_on_drop(&trial);
    assert_eq!(bearer.as_str(), "Bearer commercial-token-contract-canary");
    assert_eq!(trial.as_str(), "CtxTrial trial-token-contract-canary");
}

fn trial_entitlement() -> SignedEntitlement {
    SignedEntitlement {
        grant: EntitlementGrant {
            schema_version: ENTITLEMENT_SCHEMA_VERSION,
            issuer: "https://commercial.ctx.test".to_owned(),
            key_id: "test-key".to_owned(),
            grant_id: "grant-123".to_owned(),
            subject: "subject-123".to_owned(),
            account_id: "account-123".to_owned(),
            product: "ctx-pro".to_owned(),
            access_kind: EntitlementAccessKind::Trial,
            installation_key_thumbprint: "thumbprint".to_owned(),
            issued_at_unix: 1_900_000_000,
            not_before_unix: 1_900_000_000,
            refresh_after_unix: 1_900_000_100,
            access_deadline_unix: 2_000_000_000,
            grace_deadline_unix: 2_000_000_000,
            expires_at_unix: 1_900_604_800,
            minimum_helper_protocol: ctx_pro_host_protocol::PROTOCOL_VERSION,
            revocation_epoch: 0,
            capabilities: Default::default(),
        },
        signature_base64url: "entitlement-signature-canary".to_owned(),
    }
}

#[test]
fn trial_referral_claim_fields_are_optional_bounded_and_typed() {
    for token in [None, Some("claim.opaque_123456".to_owned())] {
        if let Some(token) = token.as_deref() {
            validate_referral_claim_token(token).unwrap();
        }
    }
    for invalid in ["short", "claim with spaces 123", "claim\nsecret_123456"] {
        assert!(validate_referral_claim_token(invalid).is_err());
    }
}

#[test]
fn referral_responses_reject_unbounded_or_inconsistent_server_data() {
    for body in [
        serde_json::json!({
            "codename": "agent-smith",
            "disposition": "created",
            "unexpected": true,
        }),
        serde_json::json!({
            "codename": "agent-smith",
            "attributed": 4,
            "subscribed": 3,
            "earned_cents": 11000,
            "pending_cents": 1000,
            "manual_review_cents": 0,
            "payable_cents": 3000,
            "processing_cents": 0,
            "paid_cents": 7000,
            "debt_cents": 0,
            "currency": "usd",
            "payout_state": "eligible",
            "unexpected": true,
        }),
        serde_json::json!({
            "kind": "payout_onboarding_created",
            "payout_state": "onboarding_pending",
            "url": "https://connect.stripe.com/setup/s/test",
            "expires_at_unix": unix_time().unwrap() + 300,
            "unexpected": true,
        }),
    ] {
        assert!(serde_json::from_value::<ReferralCreateResult>(body.clone()).is_err());
        assert!(serde_json::from_value::<ReferralStatusResult>(body.clone()).is_err());
        assert!(serde_json::from_value::<ReferralPayoutResult>(body).is_err());
    }

    let complete_status = serde_json::json!({
        "codename": "agent-smith",
        "attributed": 4,
        "subscribed": 3,
        "earned_cents": 11000,
        "pending_cents": 1000,
        "manual_review_cents": 0,
        "payable_cents": 3000,
        "processing_cents": 0,
        "paid_cents": 7000,
        "debt_cents": 0,
        "currency": "usd",
        "payout_state": "eligible",
    });
    for required_field in ["manual_review_cents", "processing_cents", "debt_cents"] {
        let mut missing = complete_status.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove(required_field)
            .unwrap();
        assert!(
            serde_json::from_value::<ReferralStatusResult>(missing).is_err(),
            "{required_field} must be required"
        );
    }

    let create = ReferralCreateResult {
        codename: "agent-smith".to_owned(),
        disposition: "created".to_owned(),
    };
    create.validate("agent-smith").unwrap();
    assert!(!format!("{create:?}").contains("agent-smith"));
    assert!(ReferralCreateResult {
        codename: "Agent Smith".to_owned(),
        disposition: "created".to_owned(),
    }
    .validate("agent-smith")
    .is_err());
    assert!(ReferralCreateResult {
        codename: "agent-smith".to_owned(),
        disposition: "renamed".to_owned(),
    }
    .validate("agent-smith")
    .is_err());
    assert!(ReferralCreateResult {
        codename: "other-agent".to_owned(),
        disposition: "created".to_owned(),
    }
    .validate("agent-smith")
    .is_err());

    let status = ReferralStatusResult {
        codename: "agent-smith".to_owned(),
        attributed: 4,
        subscribed: 3,
        earned_cents: 11_000,
        pending_cents: 1_000,
        manual_review_cents: 0,
        payable_cents: 3_000,
        processing_cents: 0,
        paid_cents: 7_000,
        debt_cents: 0,
        currency: "usd".to_owned(),
        payout_state: "eligible".to_owned(),
    };
    status.validate().unwrap();
    assert!(!format!("{status:?}").contains("agent-smith"));
    let invalid_statuses = [
        ReferralStatusResult {
            subscribed: 5,
            ..status.clone()
        },
        ReferralStatusResult {
            currency: "eur".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            earned_cents: MAX_REFERRAL_CENTS + 1,
            ..status.clone()
        },
        ReferralStatusResult {
            payout_state: "secret_upstream_state".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            earned_cents: 11_001,
            ..status.clone()
        },
        ReferralStatusResult {
            earned_cents: 10_000,
            ..status.clone()
        },
        ReferralStatusResult {
            subscribed: 0,
            ..status.clone()
        },
        ReferralStatusResult {
            pending_cents: 0,
            manual_review_cents: 1_000,
            payout_state: "eligible".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            payable_cents: 0,
            pending_cents: 4_000,
            payout_state: "eligible".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            payable_cents: 2_000,
            processing_cents: 1_000,
            payout_state: "eligible".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            payout_state: "paused".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            earned_cents: 0,
            pending_cents: 0,
            payable_cents: 0,
            processing_cents: 1_000,
            paid_cents: 2_000,
            debt_cents: 3_000,
            payout_state: "paused".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            attributed: 1,
            subscribed: 1,
            earned_cents: 1_500,
            pending_cents: 0,
            manual_review_cents: 0,
            payable_cents: 0,
            processing_cents: 0,
            paid_cents: 2_000,
            debt_cents: 500,
            payout_state: "paused".to_owned(),
            ..status.clone()
        },
        ReferralStatusResult {
            attributed: 1,
            subscribed: 1,
            earned_cents: 13_000,
            pending_cents: 13_000,
            manual_review_cents: 0,
            payable_cents: 0,
            processing_cents: 0,
            paid_cents: 0,
            debt_cents: 0,
            payout_state: "not_eligible".to_owned(),
            ..status
        },
    ];
    for invalid in invalid_statuses {
        assert!(invalid.validate().is_err());
    }

    for valid in [
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 4,
            subscribed: 4,
            earned_cents: 8_000,
            pending_cents: 2_000,
            manual_review_cents: 2_000,
            payable_cents: 2_000,
            processing_cents: 0,
            paid_cents: 2_000,
            debt_cents: 0,
            currency: "usd".to_owned(),
            payout_state: "paused".to_owned(),
        },
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 4,
            subscribed: 3,
            earned_cents: 4_000,
            pending_cents: 2_000,
            manual_review_cents: 0,
            payable_cents: 0,
            processing_cents: 0,
            paid_cents: 2_000,
            debt_cents: 0,
            currency: "usd".to_owned(),
            payout_state: "not_eligible".to_owned(),
        },
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 1,
            subscribed: 1,
            earned_cents: 1_000,
            pending_cents: 0,
            manual_review_cents: 0,
            payable_cents: 1_000,
            processing_cents: 0,
            paid_cents: 0,
            debt_cents: 0,
            currency: "usd".to_owned(),
            payout_state: "onboarding_pending".to_owned(),
        },
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 1,
            subscribed: 1,
            earned_cents: 1_000,
            pending_cents: 0,
            manual_review_cents: 0,
            payable_cents: 1_000,
            processing_cents: 0,
            paid_cents: 0,
            debt_cents: 0,
            currency: "usd".to_owned(),
            payout_state: "ready".to_owned(),
        },
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 1,
            subscribed: 1,
            earned_cents: 1_000,
            pending_cents: 0,
            manual_review_cents: 0,
            payable_cents: 0,
            processing_cents: 0,
            paid_cents: 2_000,
            debt_cents: 1_000,
            currency: "usd".to_owned(),
            payout_state: "paused".to_owned(),
        },
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 1,
            subscribed: 1,
            earned_cents: 4_000,
            pending_cents: 0,
            manual_review_cents: 0,
            payable_cents: 0,
            processing_cents: 2_000,
            paid_cents: 2_000,
            debt_cents: 0,
            currency: "usd".to_owned(),
            payout_state: "paused".to_owned(),
        },
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 1,
            subscribed: 1,
            earned_cents: MAX_REFERRAL_CENTS_PER_ATTRIBUTION,
            pending_cents: MAX_REFERRAL_CENTS_PER_ATTRIBUTION,
            manual_review_cents: 0,
            payable_cents: 0,
            processing_cents: 0,
            paid_cents: 0,
            debt_cents: 0,
            currency: "usd".to_owned(),
            payout_state: "not_eligible".to_owned(),
        },
    ] {
        valid.validate().unwrap();
    }

    let now = unix_time().unwrap();
    let payout = ReferralPayoutResult {
        kind: "payout_onboarding_created".to_owned(),
        payout_state: "onboarding_pending".to_owned(),
        url: "https://connect.stripe.com/setup/s/test".to_owned(),
        expires_at_unix: now + 300,
    };
    payout.validate().unwrap();
    assert!(!format!("{payout:?}").contains("connect.stripe.com"));
    for url in [
        "http://connect.stripe.com/setup/s/test",
        "https://stripe.example.test/setup",
        "file:///tmp/payout",
    ] {
        assert!(ReferralPayoutResult {
            kind: "payout_onboarding_created".to_owned(),
            payout_state: "onboarding_pending".to_owned(),
            url: url.to_owned(),
            expires_at_unix: now + 300,
        }
        .validate()
        .is_err());
    }
}

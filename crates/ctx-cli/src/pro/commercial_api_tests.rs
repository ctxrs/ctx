use super::*;

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
        trial_authorization("abcDEF_123-456.xyz~").unwrap(),
        "CtxTrial abcDEF_123-456.xyz~"
    );
    for invalid in ["short", "contains space token", "line\nbreak"] {
        assert!(trial_authorization(invalid).is_err());
    }

    let challenge = TrialChallenge {
        challenge_id: "challenge-123".to_owned(),
        challenge_base64url: "a".repeat(43),
        expires_at_unix: unix_time().unwrap() + 60,
        artifact_access_token: "a".repeat(32),
    };
    challenge.validate().unwrap();

    let expired = TrialChallenge {
        expires_at_unix: unix_time().unwrap() - 1,
        ..challenge
    };
    assert!(expired.validate().is_err());
}

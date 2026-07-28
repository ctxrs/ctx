use std::cell::{Cell, RefCell};

use crate::pro::commercial_api::BillingState;

use super::*;

fn elapsed_since(clock: &Cell<i64>, start: i64) -> Duration {
    Duration::from_secs(u64::try_from(clock.get() - start).unwrap())
}

#[test]
fn only_terminal_anonymous_trial_states_enter_paid_conversion() {
    for code in [
        "anonymous_trial_already_consumed",
        "anonymous_trial_identity_ambiguous",
        "anonymous_trial_installation_limit",
        "commercial_access_locked",
    ] {
        assert!(anonymous_trial_requires_conversion(&anyhow!(
            "{code}: denied"
        )));
    }
    for code in ["service_unavailable", "rate_limited", "invalid_response"] {
        assert!(!anonymous_trial_requires_conversion(&anyhow!(
            "{code}: failed"
        )));
    }
}

#[test]
fn trial_only_never_enters_paid_auth_with_an_existing_session() {
    let anonymous_calls = Cell::new(0);
    let paid_calls = Cell::new(0);
    let error = setup_with_access_policy(
        true,
        false,
        Some(EntitlementAccessKind::Active),
        || {
            anonymous_calls.set(anonymous_calls.get() + 1);
            bail!("anonymous_trial_identity_ambiguous: denied")
        },
        || {
            paid_calls.set(paid_calls.get() + 1);
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(
        crate::pro::stable_error_code(&error),
        Some("anonymous_trial_identity_ambiguous")
    );
    assert_eq!(anonymous_calls.get(), 1);
    assert_eq!(paid_calls.get(), 0);
}

fn commercial_state(access_state: &str, access_deadline_unix: Option<i64>) -> CommercialState {
    CommercialState {
        subject: "user_123".to_owned(),
        account_id: "org_123".to_owned(),
        access_state: access_state.to_owned(),
        access_deadline_unix,
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
    }
}

#[test]
fn already_subscribed_uses_embedded_state_without_a_second_account_request() {
    let account_calls = Cell::new(0_u32);
    let checkout_calls = Cell::new(0_u32);
    let await_calls = Cell::new(0_u32);
    let initial = commercial_state("none", None);
    let subscribed = commercial_state("active", Some(2_000));
    let mut context = ();

    let state = resolve_active_state(
        &mut context,
        |_| {
            account_calls.set(account_calls.get() + 1);
            if account_calls.get() > 1 {
                bail!("unexpected second account request");
            }
            Ok(initial.clone())
        },
        |_| {
            checkout_calls.set(checkout_calls.get() + 1);
            Ok(CheckoutResult {
                kind: "already_subscribed".to_owned(),
                url: None,
                expires_at_unix: None,
                state: Some(subscribed.clone()),
            })
        },
        |_, _, _| {
            await_calls.set(await_calls.get() + 1);
            bail!("Checkout polling must not run for an existing subscription")
        },
    )
    .unwrap();

    assert_eq!(state.access_state, "active");
    assert_eq!(account_calls.get(), 1);
    assert_eq!(checkout_calls.get(), 1);
    assert_eq!(await_calls.get(), 0);
}

#[test]
fn url_less_pending_checkout_uses_the_polling_path() {
    let await_calls = Cell::new(0_u32);
    let initial = commercial_state("none", None);
    let active = commercial_state("active", Some(2_000));
    let mut context = ();

    let state = resolve_active_state(
        &mut context,
        |_| Ok(initial.clone()),
        |_| {
            Ok(CheckoutResult {
                kind: "checkout_pending".to_owned(),
                url: None,
                expires_at_unix: Some(1_500),
                state: None,
            })
        },
        |_, observed, checkout| {
            await_calls.set(await_calls.get() + 1);
            assert_eq!(observed.access_state, "none");
            assert_eq!(checkout.kind, "checkout_pending");
            assert!(checkout.url.is_none());
            assert_eq!(checkout.expires_at_unix, Some(1_500));
            Ok(active.clone())
        },
    )
    .unwrap();

    assert_eq!(state.access_state, "active");
    assert_eq!(await_calls.get(), 1);
}

#[test]
fn checkout_poll_refreshes_an_expired_token_during_a_long_poll() {
    let clock = Cell::new(1_000_i64);
    let account_calls = Cell::new(0_u32);
    let refresh_calls = Cell::new(0_u32);
    let initial = commercial_state("none", None);
    let active = commercial_state("active", Some(2_000));
    let mut access_token = "original-access-token".to_owned();

    let state = poll_checkout_access(
        2_000,
        || Ok(clock.get()),
        || elapsed_since(&clock, 1_000),
        |duration| {
            clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
        },
        || {
            poll_checkout_account(
                &mut access_token,
                &initial,
                |observed_access_token| {
                    account_calls.set(account_calls.get() + 1);
                    if observed_access_token == "original-access-token" {
                        if clock.get() < 1_360 {
                            Ok(initial.clone())
                        } else {
                            bail!("authentication_required: access token expired")
                        }
                    } else if observed_access_token == "refreshed-access-token" {
                        Ok(active.clone())
                    } else {
                        bail!("invalid_response: unexpected access token")
                    }
                },
                |rejected_access_token| {
                    refresh_calls.set(refresh_calls.get() + 1);
                    assert_eq!(rejected_access_token, "original-access-token");
                    Ok("refreshed-access-token".to_owned())
                },
            )
        },
        |_| {},
    )
    .unwrap();

    assert_eq!(state.access_state, "active");
    assert!(clock.get() >= 1_360);
    assert_eq!(refresh_calls.get(), 1);
    assert_eq!(access_token, "refreshed-access-token");
    assert!(account_calls.get() > 2);
}

#[test]
fn checkout_poll_retries_transient_refresh_failures() {
    let clock = Cell::new(1_000_i64);
    let refresh_calls = Cell::new(0_u32);
    let initial = commercial_state("none", None);
    let active = commercial_state("active", Some(2_000));
    let mut access_token = "expired-access-token".to_owned();

    let state = poll_checkout_access(
        2_000,
        || Ok(clock.get()),
        || elapsed_since(&clock, 1_000),
        |duration| {
            clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
        },
        || {
            poll_checkout_account(
                &mut access_token,
                &initial,
                |observed_access_token| {
                    if observed_access_token == "fresh-access-token" {
                        Ok(active.clone())
                    } else {
                        bail!("authentication_required: access token expired")
                    }
                },
                |_| {
                    refresh_calls.set(refresh_calls.get() + 1);
                    if refresh_calls.get() == 1 {
                        bail!("service_unavailable: WorkOS token refresh failed")
                    }
                    Ok("fresh-access-token".to_owned())
                },
            )
        },
        |_| {},
    )
    .unwrap();

    assert_eq!(state.access_state, "active");
    assert_eq!(refresh_calls.get(), 2);
    assert_eq!(access_token, "fresh-access-token");
}

#[test]
fn checkout_poll_stops_after_one_fatal_refresh_failure() {
    let clock = Cell::new(1_000_i64);
    let account_calls = Cell::new(0_u32);
    let refresh_calls = Cell::new(0_u32);
    let initial = commercial_state("none", None);
    let mut access_token = "expired-access-token".to_owned();

    let error = poll_checkout_access::<CommercialState>(
        2_000,
        || Ok(clock.get()),
        || elapsed_since(&clock, 1_000),
        |duration| {
            clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
        },
        || {
            poll_checkout_account(
                &mut access_token,
                &initial,
                |_| {
                    account_calls.set(account_calls.get() + 1);
                    bail!("authentication_required: access token expired")
                },
                |_| {
                    refresh_calls.set(refresh_calls.get() + 1);
                    bail!("authentication_failed: WorkOS refresh token was rejected")
                },
            )
        },
        |_| {},
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "authentication_failed: WorkOS refresh token was rejected"
    );
    assert_eq!(clock.get(), 1_000);
    assert_eq!(account_calls.get(), 1);
    assert_eq!(refresh_calls.get(), 1);
    assert_eq!(access_token, "expired-access-token");
}

#[test]
fn checkout_poll_rejects_identity_changes_after_token_refresh() {
    let initial = commercial_state("none", None);
    let mut changed = commercial_state("active", Some(2_000));
    changed.subject = "user_456".to_owned();
    let mut access_token = "expired-access-token".to_owned();

    let outcome = poll_checkout_account(
        &mut access_token,
        &initial,
        |observed_access_token| {
            if observed_access_token == "fresh-access-token" {
                Ok(changed.clone())
            } else {
                bail!("authentication_required: access token expired")
            }
        },
        |_| Ok("fresh-access-token".to_owned()),
    );

    let CheckoutPoll::Fatal(error) = outcome else {
        panic!("identity changes must fail closed");
    };
    assert_eq!(
        error.to_string(),
        "invalid_response: commercial account changed during Checkout"
    );
    assert_eq!(access_token, "fresh-access-token");
}

#[test]
fn key_store_error_vocabulary_is_exact_and_does_not_expose_old_tokens() {
    let cases = [
        (
            CredentialVaultError::Unavailable { platform: "linux" },
            "key_store_unavailable:",
        ),
        (CredentialVaultError::Corrupt, "key_store_unavailable:"),
        (CredentialVaultError::Locked, "key_store_locked:"),
    ];
    for (error, expected) in cases {
        let rendered = vault_error(error).to_string();
        assert!(rendered.starts_with(expected));
        assert!(!rendered.contains("credential_vault_"));
    }
}

#[test]
fn browser_urls_are_validated_before_launch() {
    assert!(validate_https_url("file:///tmp/session", "Checkout").is_err());
    assert!(validate_https_url("https://checkout.stripe.com/session", "Checkout").is_ok());
}

#[test]
fn checkout_poll_uses_bounded_adaptive_backoff_without_real_sleep() {
    let clock = Cell::new(1_000_i64);
    let sleeps = RefCell::new(Vec::new());
    let attempts = Cell::new(0_u32);
    let result = poll_checkout_access(
        2_000,
        || Ok(clock.get()),
        || elapsed_since(&clock, 1_000),
        |duration| {
            sleeps.borrow_mut().push(duration);
            clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
        },
        || {
            let attempt = attempts.get() + 1;
            attempts.set(attempt);
            match attempt {
                1 => CheckoutPoll::Retryable(None),
                2..=5 => CheckoutPoll::Pending,
                _ => CheckoutPoll::Granted("active"),
            }
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(result, "active");
    assert_eq!(attempts.get(), 6);
    assert_eq!(
        sleeps.into_inner(),
        [
            Duration::from_secs(3),
            Duration::from_secs(6),
            Duration::from_secs(12),
            Duration::from_secs(15),
            Duration::from_secs(15),
        ]
    );
}

#[test]
fn checkout_poll_respects_retry_after_across_progress_wakes() {
    let clock = Cell::new(1_000_i64);
    let sleeps = RefCell::new(Vec::new());
    let attempts = Cell::new(0_u32);
    let result = poll_checkout_access(
        2_000,
        || Ok(clock.get()),
        || elapsed_since(&clock, 1_000),
        |duration| {
            sleeps.borrow_mut().push(duration);
            clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
        },
        || {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                CheckoutPoll::Retryable(Some(Duration::from_secs(120)))
            } else {
                CheckoutPoll::Granted("active")
            }
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(result, "active");
    assert_eq!(attempts.get(), 2);
    assert_eq!(
        sleeps.into_inner(),
        [Duration::from_secs(60), Duration::from_secs(60)]
    );
}

#[test]
fn checkout_poll_stops_at_expiry_or_thirty_minutes() {
    for (expires_at, expected_error, expected_elapsed) in [
        (1_005, "checkout_expired:", 5_i64),
        (
            5_000,
            "checkout_timeout:",
            i64::try_from(CHECKOUT_POLL_MAX_SECONDS).unwrap(),
        ),
    ] {
        let clock = Cell::new(1_000_i64);
        let attempts = Cell::new(0_u32);
        let error = poll_checkout_access::<()>(
            expires_at,
            || Ok(clock.get()),
            || elapsed_since(&clock, 1_000),
            |duration| {
                clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
            },
            || {
                attempts.set(attempts.get() + 1);
                CheckoutPoll::Pending
            },
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().starts_with(expected_error), "{error:#}");
        assert!(error.to_string().contains("rerun `ctx pro`"));
        assert_eq!(clock.get() - 1_000, expected_elapsed);
        assert!(attempts.get() > 0);
    }
}

#[test]
fn checkout_poll_rejects_a_grant_returned_after_the_deadline() {
    let clock = Cell::new(1_000_i64);
    let error = poll_checkout_access(
        1_005,
        || Ok(clock.get()),
        || elapsed_since(&clock, 1_000),
        |_| {},
        || {
            clock.set(1_005);
            CheckoutPoll::Granted("late grant")
        },
        |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("checkout_expired:"));
}

#[test]
fn checkout_poll_timeout_is_monotonic_when_the_wall_clock_moves_backward() {
    let wall_clock = Cell::new(1_000_i64);
    let elapsed = Cell::new(Duration::ZERO);
    let error = poll_checkout_access::<()>(
        5_000,
        || Ok(wall_clock.get()),
        || elapsed.get(),
        |duration| {
            elapsed.set(elapsed.get().saturating_add(duration));
            wall_clock.set(wall_clock.get().saturating_sub(1));
        },
        || CheckoutPoll::Pending,
        |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("checkout_timeout:"));
    assert_eq!(
        elapsed.get(),
        Duration::from_secs(CHECKOUT_POLL_MAX_SECONDS)
    );
}

#[test]
fn checkout_poll_fails_closed_and_reports_bounded_progress() {
    let clock = Cell::new(1_000_i64);
    let progress = RefCell::new(Vec::new());
    let error = poll_checkout_access::<()>(
        2_000,
        || Ok(clock.get()),
        || elapsed_since(&clock, 1_000),
        |duration| {
            clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
        },
        || {
            if clock.get() < 1_063 {
                CheckoutPoll::Pending
            } else {
                CheckoutPoll::Fatal(anyhow!("authentication_required: sign in again"))
            }
        },
        |elapsed| progress.borrow_mut().push(elapsed),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "authentication_required: sign in again");
    assert_eq!(progress.into_inner(), [60]);
    assert_eq!(clock.get(), 1_066);
}

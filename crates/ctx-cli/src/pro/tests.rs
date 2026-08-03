use std::io::Write as _;

use unicode_width::UnicodeWidthStr as _;

use super::*;

fn strip_ansi(rendered: &str) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    stream.write_all(rendered.as_bytes()).unwrap();
    String::from_utf8(stream.into_inner()).unwrap()
}

fn context(width: usize, color: crate::ui::ColorMode) -> RenderContext {
    RenderContext::for_test(
        crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, width).color(color),
    )
}

#[test]
fn timeout_human_copy_is_exact_in_plain_and_ansi_modes() {
    for (raw, expected_plain, expected_ansi) in [
            (
                "helper_timeout: untrusted helper detail",
                "✗ The ctx Pro helper did not respond in time\nNo helper response was accepted.\n\nHint: Try the command again.\n\nNext\n  ctx pro\n",
                "\u{1b}[31m✗\u{1b}[0m \u{1b}[1mThe ctx Pro helper did not respond in time\u{1b}[0m\nNo helper response was accepted.\n\n\u{1b}[2mHint\u{1b}[0m: Try the command again.\n\n\u{1b}[2mNext\u{1b}[0m\n  \u{1b}[36mctx pro\u{1b}[0m\n",
            ),
            (
                "checkout_timeout: untrusted checkout detail",
                "✗ Checkout did not finish in time\nThe checkout may still be open, but ctx stopped waiting after 30 minutes.\n\nHint: Resume ctx Pro setup to check access or continue checkout.\n\nNext\n  ctx pro\n",
                "\u{1b}[31m✗\u{1b}[0m \u{1b}[1mCheckout did not finish in time\u{1b}[0m\nThe checkout may still be open, but ctx stopped waiting after 30 minutes.\n\n\u{1b}[2mHint\u{1b}[0m: Resume ctx Pro setup to check access or continue checkout.\n\n\u{1b}[2mNext\u{1b}[0m\n  \u{1b}[36mctx pro\u{1b}[0m\n",
            ),
        ] {
            let plain_context = context(120, crate::ui::ColorMode::Never);
            let plain = human_actionable_error_document(
                &plain_context,
                &anyhow!(raw),
                "ctx pro",
            )
            .unwrap()
            .render(&plain_context);
            assert_eq!(plain, expected_plain);

            let ansi_context = context(120, crate::ui::ColorMode::Always);
            let ansi = human_actionable_error_document(
                &ansi_context,
                &anyhow!(raw),
                "ctx pro",
            )
            .unwrap()
            .render(&ansi_context);
            assert_eq!(ansi, expected_ansi);
            assert_eq!(strip_ansi(&ansi), expected_plain);
            assert!(!plain.contains("untrusted"));
            assert!(!ansi.contains("untrusted"));
        }
}

#[test]
fn human_helper_failures_use_safe_semantic_copyable_retry_actions() {
    for (raw, expected, action) in [
        (
            "helper_crashed: /private/helper/path",
            "The ctx Pro helper stopped unexpectedly",
            "ctx pro",
        ),
        (
            "helper_timeout: secret helper stderr",
            "The ctx Pro helper did not respond in time",
            "ctx pro",
        ),
        (
            "protocol_mismatch: helper build 123",
            "The ctx Pro helper needs repair",
            "ctx pro",
        ),
        (
            "invalid_response: malformed helper payload",
            "ctx Pro returned an invalid response",
            "ctx pro",
        ),
        (
            "source_unavailable: /private/source/path",
            "Local history is not ready",
            "ctx setup",
        ),
    ] {
        for width in [32, 48, 80, 120] {
            let context = RenderContext::for_test(
                crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, width)
                    .color(crate::ui::ColorMode::Always),
            );
            let document =
                human_actionable_error_document(&context, &anyhow!(raw), "ctx pro").unwrap();
            let plain = document.render_plain();
            let styled = document.render(&context);
            let normalized = plain.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(plain.starts_with("✗ "), "{plain}");
            assert!(normalized.contains(expected), "{plain}");
            assert!(plain.contains(&format!("\nNext\n  {action}\n")), "{plain}");
            assert!(!plain.lines().next().unwrap_or_default().contains("ctx pro"));
            assert!(!plain.contains("secret"));
            assert!(!plain.contains("/private"));
            assert!(!plain.contains("helper_"));
            if raw.starts_with("source_unavailable") {
                assert!(
                    normalized.contains("Index local agent history before retrying this command.")
                );
                assert!(!normalized.contains("ctx blame"));
            }
            assert!(
                styled.contains(&format!("\u{1b}[36m{action}")),
                "{styled:?}"
            );
            assert_eq!(strip_ansi(&styled), plain);
            let maximum = context.content_width().unwrap();
            assert!(plain
                .lines()
                .all(|line| { line.width() <= maximum || line.trim_start().starts_with("ctx ") }));
        }
    }
}

#[test]
fn lifecycle_failures_keep_safe_causes_and_replace_terminal_sign_in_actions() {
    for (raw, retry, title, detail, action) in [
            (
                "key_store_locked",
                "ctx pro",
                "The secure key store is locked",
                "Unlock the selected persistent key store",
                "ctx pro",
            ),
            (
                "authentication_expired",
                "ctx pro",
                "ctx Pro sign-in expired",
                "sign-in link and code are no longer valid",
                "ctx pro",
            ),
            (
                "service_unavailable: WorkOS device authorization failed",
                "ctx pro",
                "The ctx Pro sign-in service is temporarily unavailable",
                "previous sign-in attempt cannot be completed",
                "ctx pro",
            ),
            (
                "commercial_identity_conflict: the billing customer belongs to a different signed-in account",
                "ctx pro",
                "This billing account belongs to another ctx Pro sign-in",
                "original signed-in account",
                "ctx pro",
            ),
            (
                "invalid_response: commercial account changed during Checkout",
                "ctx pro",
                "Checkout used a different ctx Pro account",
                "Access was not granted",
                "ctx pro",
            ),
            (
                "protocol_mismatch: referral helper used an old protocol",
                "ctx referral status",
                "The ctx Pro helper needs repair",
                "compatible signed helper",
                "ctx pro",
            ),
        ] {
            let document =
                human_actionable_error_document(&context(48, crate::ui::ColorMode::Always), &anyhow!(raw), retry)
                    .unwrap();
            let plain = document.render_plain();
            let styled = document.render(&context(48, crate::ui::ColorMode::Always));
            let normalized = plain.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(plain.starts_with("✗ "), "{plain}");
            assert!(normalized.contains(title), "{plain}");
            assert!(normalized.contains(detail), "{plain}");
            assert!(plain.contains(&format!("Next\n  {action}\n")), "{plain}");
            assert_eq!(strip_ansi(&styled), plain);
            assert!(!plain.contains("commercial_identity_conflict"), "{plain}");
            assert!(!plain.contains("protocol_mismatch"), "{plain}");
        }
}

#[test]
fn referral_failures_use_operation_specific_safe_next_actions() {
    for (raw, retry, title, detail, action) in [
        (
            "rate_limited: the commercial service is rate limited",
            "ctx referral status",
            "The referral service is temporarily rate limited",
            "No referral or payout state was changed",
            "ctx referral status",
        ),
        (
            "service_unavailable: commercial API request failed",
            "ctx referral status",
            "Referral status is temporarily unavailable",
            "Local ctx data is unchanged",
            "ctx referral status",
        ),
        (
            "referral_codename_conflict: this account already has a different referral codename",
            "ctx referral create other-agent",
            "This account already has a different referral codename",
            "cannot replace it",
            "ctx referral status",
        ),
        (
            "invalid_request: the referral codename is reserved",
            "ctx referral create admin",
            "That referral codename is reserved",
            "Choose a different available codename",
            "ctx referral create <codename>",
        ),
        (
            "authentication_required: a verified WorkOS email is required for referrals",
            "ctx referral create agent-smith",
            "A verified WorkOS email is required for referrals",
            "Verify the email",
            "ctx referral create agent-smith",
        ),
        (
            "referral_not_found: create a referral codename first",
            "ctx referral status",
            "No referral codename exists for this account",
            "Create a stable codename",
            "ctx referral create <codename>",
        ),
        (
            "referral_not_eligible: a payable referral balance is required",
            "ctx referral payout --no-open",
            "Referral payout setup is not available yet",
            "payable referral balance is required",
            "ctx referral status",
        ),
        (
            "referral_payout_unavailable: referral payout onboarding is not currently available",
            "ctx referral payout --no-open",
            "Referral payout setup is temporarily unavailable",
            "could not start Stripe-hosted payout onboarding",
            "ctx referral payout --no-open",
        ),
        (
            "invalid_response: referral creation result is invalid",
            "ctx referral create agent-smith",
            "Referral creation returned an invalid response",
            "No codename was accepted",
            "ctx referral create agent-smith",
        ),
        (
            "invalid_response: commercial API error response is not contracted JSON",
            "ctx referral status",
            "The referral service returned an invalid response",
            "service error failed validation",
            "ctx referral status",
        ),
        (
            "invalid_response: referral payout result is invalid",
            "ctx referral payout --no-open",
            "Referral payout returned an invalid response",
            "No onboarding link was accepted",
            "ctx referral payout --no-open",
        ),
    ] {
        let document = human_actionable_error_document(
            &context(80, crate::ui::ColorMode::Never),
            &anyhow!(raw),
            retry,
        )
        .unwrap();
        let rendered = document.render_plain();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(rendered.starts_with("✗ "), "{rendered}");
        assert!(normalized.contains(title), "{rendered}");
        assert!(normalized.contains(detail), "{rendered}");
        assert!(
            rendered.contains(&format!("Next\n  {action}\n")),
            "{rendered}"
        );
        assert!(!rendered.contains("Resolve the issue"), "{rendered}");
    }
}

#[test]
fn code_only_human_failures_use_safe_command_specific_recovery() {
    for (raw, retry, title, action) in [
        (
            "checkout_expired",
            "ctx pro",
            "Checkout expired before access was granted",
            "ctx pro",
        ),
        (
            "checkout_timeout",
            "ctx pro",
            "Checkout did not finish in time",
            "ctx pro",
        ),
        (
            "commercial_identity_conflict",
            "ctx pro",
            "Checkout used a different ctx Pro account",
            "ctx pro",
        ),
        (
            "commercial_identity_conflict",
            "ctx pro manage --no-open",
            "This billing account belongs to another ctx Pro sign-in",
            "ctx pro",
        ),
        (
            "service_unavailable",
            "ctx pro",
            "ctx Pro is temporarily unavailable",
            "ctx pro",
        ),
        (
            "service_unavailable",
            "ctx pro manage --no-open",
            "ctx Pro account management is temporarily unavailable",
            "ctx pro manage --no-open",
        ),
        (
            "rate_limited",
            "ctx referral status",
            "The referral service is temporarily rate limited",
            "ctx referral status",
        ),
        (
            "service_unavailable",
            "ctx referral status",
            "Referral status is temporarily unavailable",
            "ctx referral status",
        ),
        (
            "referral_unavailable",
            "ctx referral create agent-smith",
            "Referrals are temporarily unavailable",
            "ctx referral create agent-smith",
        ),
        (
            "referral_codename_conflict",
            "ctx referral create other-agent",
            "This account already has a different referral codename",
            "ctx referral status",
        ),
        (
            "invalid_request",
            "ctx referral create admin",
            "That referral codename is unavailable",
            "ctx referral create <codename>",
        ),
        (
            "authentication_required",
            "ctx referral create agent-smith",
            "A verified ctx account is required for referrals",
            "ctx pro",
        ),
        (
            "referral_not_found",
            "ctx referral status",
            "No referral codename exists for this account",
            "ctx referral create <codename>",
        ),
        (
            "referral_not_eligible",
            "ctx referral payout --no-open",
            "Referral payout setup is not available yet",
            "ctx referral status",
        ),
        (
            "referral_payout_unavailable",
            "ctx referral payout --no-open",
            "Referral payout setup is temporarily unavailable",
            "ctx referral payout --no-open",
        ),
    ] {
        let document = human_actionable_error_document(
            &context(80, crate::ui::ColorMode::Never),
            &anyhow!(raw),
            retry,
        )
        .unwrap_or_else(|| panic!("missing presentation for {raw} with {retry}"));
        let rendered = document.render_plain();
        assert!(
            rendered.starts_with(&format!("✗ {title}")),
            "{raw}: {rendered}"
        );
        assert!(
            rendered.contains(&format!("Next\n  {action}\n")),
            "{raw}: {rendered}"
        );
        assert!(!rendered.contains(raw), "{raw}: {rendered}");
    }
}

#[test]
fn contextual_codes_use_only_bounded_sanitized_cause_details() {
    let contextual = anyhow!("referral_not_eligible: a payable referral balance is required")
        .context("payout request failed");
    let document = human_actionable_error_document(
        &context(80, crate::ui::ColorMode::Never),
        &contextual,
        "ctx referral payout --no-open",
    )
    .unwrap();
    let rendered = document.render_plain();
    assert!(rendered.contains("Referral payout setup is not available yet"));
    assert!(rendered.contains("Next\n  ctx referral status\n"));
    assert!(!rendered.contains("payout request failed"));

    for unsafe_detail in [
        format!("referral_not_eligible: {}", "x".repeat(257)),
        "referral_not_eligible: line one\nline two".to_owned(),
    ] {
        let rendered = human_actionable_error_document(
            &context(80, crate::ui::ColorMode::Never),
            &anyhow::Error::msg(unsafe_detail),
            "ctx referral payout --no-open",
        )
        .unwrap()
        .render_plain();
        assert!(rendered.contains("Referral payout setup is not available yet"));
        assert!(!rendered.contains("xxxx"));
        assert!(!rendered.contains("line one"));
    }
}

#[test]
fn machine_errors_preserve_exact_bytes_for_repaired_human_families() {
    let pipe = RenderContext::for_test(crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr));
    let mut ui = Ui::with_writers(std::io::sink(), pipe, std::io::sink(), pipe);
    for raw in [
        "authentication_denied: WorkOS sign-in was denied",
        "checkout_expired",
        "checkout_timeout",
        "service_unavailable",
        "rate_limited",
        "referral_codename_conflict",
        "referral_not_found",
        "referral_not_eligible",
        "referral_payout_unavailable",
        "referral_unavailable",
        "pro_not_installed: no Pro helper at /private/helper",
        "entitlement_expired: private entitlement detail",
        "key_store_unavailable: private key-store detail",
        "key_store_locked: selected store is locked",
        "cancelled: uninstall confirmation was not provided",
        "helper_upgrade_required: private helper detail",
        "commercial_identity_conflict: bounded safe cause",
        "authentication_expired: WorkOS device authorization expired",
        "invalid_response: referral payout result is invalid",
        "protocol_mismatch: untrusted helper detail",
    ] {
        let error = human_result::<()>(Err(anyhow!(raw)), false, "ctx referral payout", &mut ui)
            .unwrap_err();
        assert_eq!(error.to_string(), raw);
    }
}

#[test]
fn stable_machine_errors_are_exact_json_without_untrusted_detail_or_ansi() {
    for (raw, code) in [
        (
            "authentication_required: token secret at /private/session",
            "authentication_required",
        ),
        (
            "referral_not_eligible: private payout ledger detail",
            "referral_not_eligible",
        ),
        (
            "referral_payout_unavailable: Stripe request id secret",
            "referral_payout_unavailable",
        ),
    ] {
        let mut output = Vec::new();
        assert!(write_stable_error_json(&mut output, &anyhow!(raw)).unwrap());
        assert_eq!(
            output,
            format!("{{\"error\":\"{code}\",\"error_code\":\"{code}\"}}\n").as_bytes()
        );
        assert!(!output.contains(&0x1b));
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["error"], code);
        assert_eq!(value["error_code"], code);
        assert!(!String::from_utf8_lossy(&output).contains("secret"));
        assert!(!String::from_utf8_lossy(&output).contains("private"));
    }

    let mut output = Vec::new();
    assert!(!write_stable_error_json(&mut output, &anyhow!("unclassified detail")).unwrap());
    assert!(output.is_empty());
}

#[test]
fn unrelated_errors_keep_their_existing_contract() {
    let error = anyhow!("authentication_required: sign in");
    let context =
        RenderContext::for_test(crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr));
    assert!(human_actionable_error_document(&context, &error, "ctx pro").is_none());
}

mod anonymous_trial;
pub(crate) mod artifact_delivery;
mod authorization;
mod client;
mod commercial_api;
mod commercial_config;
mod commercial_deletion;
mod commercial_lifecycle;
mod commercial_production_record;
mod credential_vault;
mod graph_key_deletion;
mod helper_command;
mod lifecycle;
mod local_deletion;
mod pending_materialization;
mod pricing;
#[cfg(any(test, ctx_pro_qualification))]
mod qualification_helper;
mod referral;
mod render;
mod request_identity;
mod setup_validation;
#[cfg(ctx_pro_test_helper)]
mod test_control;
mod verified_executable;
mod workos_device;
use crate::ui::{hint, outcome, Action, Document, Hint, Outcome, OutcomeState, RenderContext, Ui};
pub(crate) use client::{
    blame, preflight_source_manifest_materialization, stable_error_code, stable_error_diagnostic,
    sync_source_manifest_materialization, RESOURCE_NOT_FOUND_DIAGNOSTIC,
};
pub(crate) use lifecycle::{lifecycle_status_json, run_lifecycle, ProArgs};
pub(crate) use pricing::PRO_MONTHLY_PRICE_DISPLAY;
#[cfg(test)]
pub(crate) use referral::parse_referral_codename;
pub(crate) use referral::{run as run_referral, show_cta_once, ReferralArgs};
pub(crate) use render::{blame_result_json, print_blame_result};

use anyhow::{anyhow, Result};
pub(crate) const DEFAULT_BLAME_LIMIT: u32 = 20;

pub(crate) fn actionable_error(error: anyhow::Error) -> anyhow::Error {
    let Some(code) = stable_error_code(&error) else {
        return error;
    };
    let guidance = match code {
        "pro_not_installed" => "ctx Pro is not set up; run `ctx pro`",
        "entitlement_expired" => "ctx Pro is locked; run `ctx pro manage` to restore access",
        "helper_upgrade_required" | "protocol_mismatch" => {
            "the Pro helper needs repair; run `ctx pro`"
        }
        "not_materialized" | "needs_rebuild" | "partial" | "needs_resume" => {
            "the Pro graph needs repair; run `ctx pro`"
        }
        "key_store_unavailable" | "key_store_locked" => {
            "unlock or repair the already selected secure key store, then run `ctx pro`; a fresh installation can select the owner-private local vault only when the native store is genuinely unavailable, and ctx never downgrades existing state"
        }
        _ => return error,
    };
    anyhow!("{code}: {guidance}")
}

pub(crate) fn human_result<T>(
    result: Result<T>,
    human_output: bool,
    retry_command: &str,
    ui: &mut Ui,
) -> Result<T> {
    if !human_output {
        return result;
    }
    let error = match result {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    let Some(document) =
        human_actionable_error_document(ui.stderr_context(), &error, retry_command)
    else {
        return Err(error);
    };
    ui.write_stderr(&document)?;
    Err(crate::dispatch::rendered_cli_error())
}

pub(crate) fn human_blame_result<T>(
    result: Result<T>,
    human_output: bool,
    ui: &mut Ui,
) -> Result<T> {
    if !human_output {
        return result.map_err(actionable_error);
    }
    let error = match result {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    let Some(document) = human_actionable_error_document(ui.stderr_context(), &error, "ctx pro")
    else {
        return Err(actionable_error(error));
    };
    ui.write_stderr(&document)?;
    Err(crate::dispatch::rendered_cli_error())
}

fn human_actionable_error_document(
    context: &RenderContext,
    error: &anyhow::Error,
    retry_command: &str,
) -> Option<Document> {
    let code = stable_error_code(error)?;
    let signals = trusted_error_signals(error, code);
    let presentation = human_error_presentation(code, signals, retry_command)?;
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: presentation.title,
            detail: presentation.detail,
        },
    );
    if let Some(action) = presentation.action {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: presentation.hint,
            },
            Some(Action { command: action }),
        ));
    }
    Some(document)
}

const MAX_HUMAN_ERROR_DETAIL_BYTES: usize = 256;

#[derive(Debug, Default, Clone, Copy)]
struct TrustedErrorSignals {
    has_detail: bool,
    billing_customer: bool,
    workos_sign_in: bool,
    reserved_referral_codename: bool,
    verified_referral_email: bool,
    checkout_account_changed: bool,
    malformed_commercial_error: bool,
    interrupted_deletion: bool,
    unsupported_platform: bool,
}

fn trusted_error_signals(error: &anyhow::Error, code: &str) -> TrustedErrorSignals {
    let mut signals = TrustedErrorSignals::default();
    for cause in error.chain() {
        let message = cause.to_string();
        let Some(detail) = message
            .strip_prefix(code)
            .and_then(|message| message.strip_prefix(':'))
            .map(str::trim)
        else {
            continue;
        };
        if detail.is_empty()
            || detail.len() > MAX_HUMAN_ERROR_DETAIL_BYTES
            || detail.chars().any(char::is_control)
        {
            continue;
        }
        let detail = detail.to_ascii_lowercase();
        // Cause text can select a bounded, trusted presentation variant, but it is never
        // retained by `TrustedErrorSignals` or passed to the renderer.
        signals.has_detail = true;
        signals.billing_customer |= detail.contains("billing customer");
        signals.workos_sign_in |= detail.contains("workos")
            || detail.contains("device authorization")
            || detail.contains("sign-in");
        signals.reserved_referral_codename |=
            detail.contains("referral codename") && detail.contains("reserved");
        signals.verified_referral_email |=
            detail.contains("verified") && detail.contains("email") && detail.contains("referral");
        signals.checkout_account_changed |=
            detail.contains("commercial account changed during checkout");
        signals.malformed_commercial_error |= detail.contains("commercial api error response")
            || detail.contains("commercial api error is malformed");
        signals.interrupted_deletion |= detail.contains("interrupted pro deletion");
        signals.unsupported_platform |=
            detail.contains("unsupported") && detail.contains("platform");
    }
    signals
}

#[derive(Debug, Clone, Copy)]
struct HumanErrorPresentation<'a> {
    title: &'static str,
    detail: Option<&'static str>,
    hint: &'static str,
    action: Option<&'a str>,
}

fn human_error_presentation<'a>(
    code: &str,
    signals: TrustedErrorSignals,
    retry_command: &'a str,
) -> Option<HumanErrorPresentation<'a>> {
    let referral_operation = referral_operation(retry_command);

    let presentation = match code {
        "authentication_denied" => HumanErrorPresentation {
            title: "ctx Pro sign-in was denied",
            detail: Some(
                "The device authorization was rejected. No sign-in session was accepted.",
            ),
            hint: "Start a fresh ctx Pro sign-in.",
            action: Some("ctx pro"),
        },
        "pro_not_installed" => HumanErrorPresentation {
            title: "ctx Pro is not set up",
            detail: Some("The signed Pro helper is not installed."),
            hint: "Set up ctx Pro.",
            action: Some("ctx pro"),
        },
        "entitlement_expired" => HumanErrorPresentation {
            title: "ctx Pro is locked",
            detail: Some(
                "Local Pro data is preserved, but Pro queries remain unavailable until access is restored.",
            ),
            hint: "Restore ctx Pro access.",
            action: Some("ctx pro manage"),
        },
        "key_store_unavailable" if signals.interrupted_deletion => HumanErrorPresentation {
            title: "A previous ctx Pro deletion is incomplete",
            detail: Some(
                "Setup and data preservation remain blocked until secure local deletion is completed.",
            ),
            hint: "Finish deleting local Pro data.",
            action: Some("ctx pro uninstall --delete-data"),
        },
        "key_store_unavailable" => HumanErrorPresentation {
            title: "The secure key store is unavailable",
            detail: Some(
                "ctx Pro could not access its existing credentials. Repair the selected persistent key store before retrying.",
            ),
            hint: "After secure storage is available, resume ctx Pro setup.",
            action: Some("ctx pro"),
        },
        "helper_crashed" => HumanErrorPresentation {
            title: "The ctx Pro helper stopped unexpectedly",
            detail: Some("No untrusted helper output was accepted."),
            hint: "Try the command again. Setup will repair the helper if needed.",
            action: Some(retry_command),
        },
        "helper_timeout" => HumanErrorPresentation {
            title: "The ctx Pro helper did not respond in time",
            detail: Some("The request stopped at its bounded timeout."),
            hint: "Try the command again.",
            action: Some(retry_command),
        },
        "helper_upgrade_required" | "protocol_mismatch" => HumanErrorPresentation {
            title: "The ctx Pro helper needs repair",
            detail: Some("Install the compatible signed helper before retrying."),
            hint: "Repair ctx Pro.",
            action: Some("ctx pro"),
        },
        "key_store_locked" => HumanErrorPresentation {
            title: "The secure key store is locked",
            detail: Some(
                "ctx Pro could not access its existing credentials. Unlock the selected persistent key store before retrying.",
            ),
            hint: "After the key store is unlocked, resume ctx Pro setup.",
            action: Some("ctx pro"),
        },
        "checkout_expired" => HumanErrorPresentation {
            title: "Checkout expired before access was granted",
            detail: Some("The previous checkout session can no longer complete."),
            hint: "Resume ctx Pro setup to create or recover the active checkout.",
            action: Some("ctx pro"),
        },
        "checkout_timeout" => HumanErrorPresentation {
            title: "Checkout did not finish in time",
            detail: Some(
                "The checkout may still be open, but ctx stopped waiting after the bounded timeout.",
            ),
            hint: "Resume ctx Pro setup to check access or continue checkout.",
            action: Some("ctx pro"),
        },
        "commercial_identity_conflict" if signals.has_detail && signals.billing_customer =>
        {
            HumanErrorPresentation {
                title: "This billing account belongs to another ctx Pro sign-in",
                detail: Some(
                    "Use the original signed-in account before opening account management again.",
                ),
                hint: "Restart ctx Pro with the original account.",
                action: Some("ctx pro"),
            }
        }
        "commercial_identity_conflict" if retry_command.starts_with("ctx pro manage") => {
            HumanErrorPresentation {
                title: "This billing account belongs to another ctx Pro sign-in",
                detail: Some(
                    "Use the account that owns this billing customer before opening account management.",
                ),
                hint: "Restart ctx Pro with the original account.",
                action: Some("ctx pro"),
            }
        }
        "commercial_identity_conflict" => HumanErrorPresentation {
            title: "Checkout used a different ctx Pro account",
            detail: Some(
                "Access was not granted. Use the account that started Checkout before resuming setup.",
            ),
            hint: "Resume setup with the original account.",
            action: Some("ctx pro"),
        },
        "authentication_expired" => HumanErrorPresentation {
            title: "ctx Pro sign-in expired",
            detail: Some(
                "The previous sign-in link and code are no longer valid. Start a new device authorization.",
            ),
            hint: "Start a fresh ctx Pro sign-in.",
            action: Some("ctx pro"),
        },
        "cancelled" => HumanErrorPresentation {
            title: "ctx Pro uninstall was cancelled",
            detail: Some(
                "No uninstall choice was received. ctx Pro and local Pro data were left unchanged.",
            ),
            hint: "Run uninstall again and answer the confirmation prompt.",
            action: Some("ctx pro uninstall"),
        },
        "rate_limited" if referral_operation.is_some() => HumanErrorPresentation {
            title: "The referral service is temporarily rate limited",
            detail: Some("No referral or payout state was changed."),
            hint: "Wait briefly, then try again.",
            action: Some(retry_command),
        },
        "service_unavailable" if referral_operation.is_some() => {
            referral_service_unavailable(referral_operation?, retry_command)
        }
        "service_unavailable" if signals.has_detail && signals.workos_sign_in =>
        {
            HumanErrorPresentation {
                title: "The ctx Pro sign-in service is temporarily unavailable",
                detail: Some(
                    "The previous sign-in attempt cannot be completed. Check connectivity before starting a fresh authorization.",
                ),
                hint: "Start a fresh ctx Pro sign-in after connectivity returns.",
                action: Some("ctx pro"),
            }
        }
        "service_unavailable" if retry_command.starts_with("ctx pro manage") => {
            HumanErrorPresentation {
                title: "ctx Pro account management is temporarily unavailable",
                detail: Some(
                    "The billing service could not complete the request. Local Pro data is unchanged.",
                ),
                hint: "Try account management again after connectivity returns.",
                action: Some(retry_command),
            }
        }
        "service_unavailable" => HumanErrorPresentation {
            title: "ctx Pro is temporarily unavailable",
            detail: Some(
                "The service could not complete setup. No new access state was accepted.",
            ),
            hint: "Try ctx Pro setup again after connectivity returns.",
            action: Some("ctx pro"),
        },
        "referral_unavailable" => HumanErrorPresentation {
            title: "Referrals are temporarily unavailable",
            detail: Some("The ctx Pro service is not accepting referral requests right now."),
            hint: "Try the referral command again later.",
            action: Some(retry_command),
        },
        "referral_payout_unavailable" => HumanErrorPresentation {
            title: "Referral payout setup is temporarily unavailable",
            detail: Some("The ctx Pro service could not start Stripe-hosted payout onboarding."),
            hint: "Try payout setup again later.",
            action: Some(retry_command),
        },
        "referral_codename_conflict" => HumanErrorPresentation {
            title: "This account already has a different referral codename",
            detail: Some(
                "Referral codenames are stable, so retrying with another name cannot replace it.",
            ),
            hint: "Show the account's active referral codename.",
            action: Some("ctx referral status"),
        },
        "referral_not_found" => HumanErrorPresentation {
            title: "No referral codename exists for this account",
            detail: Some("Create a stable codename before checking referral status."),
            hint: "Choose and create a referral codename.",
            action: Some("ctx referral create <codename>"),
        },
        "referral_not_eligible" => HumanErrorPresentation {
            title: "Referral payout setup is not available yet",
            detail: Some(
                "A payable referral balance is required before payout onboarding can start.",
            ),
            hint: "Check the current referral balance and payout state.",
            action: Some("ctx referral status"),
        },
        "invalid_request" if signals.unsupported_platform => HumanErrorPresentation {
            title: "ctx Pro is not available on this platform",
            detail: Some("No compatible signed helper is published for this release target."),
            hint: "",
            action: None,
        },
        "invalid_request" if signals.reserved_referral_codename =>
        {
            HumanErrorPresentation {
                title: "That referral codename is reserved",
                detail: Some("Choose a different available codename."),
                hint: "Retry with a different codename.",
                action: Some("ctx referral create <codename>"),
            }
        }
        "invalid_request" if matches!(referral_operation, Some(ReferralOperation::Create)) => {
            HumanErrorPresentation {
                title: "That referral codename is unavailable",
                detail: Some("Choose a different codename that meets the referral naming rules."),
                hint: "Retry with a different codename.",
                action: Some("ctx referral create <codename>"),
            }
        }
        "authentication_required" if signals.verified_referral_email =>
        {
            HumanErrorPresentation {
                title: "A verified WorkOS email is required for referrals",
                detail: Some(
                    "Verify the email on the signed-in WorkOS account before retrying this command.",
                ),
                hint: "After the account email is verified, try again.",
                action: Some(retry_command),
            }
        }
        "authentication_required" if referral_operation.is_some() => HumanErrorPresentation {
            title: "A verified ctx account is required for referrals",
            detail: Some(
                "Sign in with a verified account before creating or managing referrals.",
            ),
            hint: "Sign in to ctx Pro, then retry the referral command.",
            action: Some("ctx pro"),
        },
        "invalid_response" if signals.checkout_account_changed =>
        {
            HumanErrorPresentation {
                title: "Checkout used a different ctx Pro account",
                detail: Some(
                    "Access was not granted. Use the account that started Checkout before resuming setup.",
                ),
                hint: "Resume setup with the original account.",
                action: Some("ctx pro"),
            }
        }
        "invalid_response" if referral_operation.is_some() => {
            referral_invalid_response(referral_operation?, signals, retry_command)
        }
        "invalid_response" => HumanErrorPresentation {
            title: "ctx Pro returned an invalid response",
            detail: Some("No untrusted service or helper result was accepted."),
            hint: "Try again later. If the failure continues, contact ctx support.",
            action: Some(retry_command),
        },
        "source_unavailable" => HumanErrorPresentation {
            title: "Local history is not ready",
            detail: Some("Index local agent history before using ctx blame."),
            hint: "Set up local history.",
            action: Some("ctx setup"),
        },
        "resource_not_found" => HumanErrorPresentation {
            title: RESOURCE_NOT_FOUND_DIAGNOSTIC,
            detail: Some("The target is valid but is not present in the materialized Pro graph."),
            hint: "",
            action: None,
        },
        _ => return None,
    };
    Some(presentation)
}

#[derive(Debug, Clone, Copy)]
enum ReferralOperation {
    Create,
    Status,
    Payout,
}

fn referral_operation(command: &str) -> Option<ReferralOperation> {
    if command.starts_with("ctx referral create") {
        Some(ReferralOperation::Create)
    } else if command.starts_with("ctx referral status") {
        Some(ReferralOperation::Status)
    } else if command.starts_with("ctx referral payout") {
        Some(ReferralOperation::Payout)
    } else {
        None
    }
}

fn referral_service_unavailable<'a>(
    operation: ReferralOperation,
    retry_command: &'a str,
) -> HumanErrorPresentation<'a> {
    let title = match operation {
        ReferralOperation::Create => "Referral creation is temporarily unavailable",
        ReferralOperation::Status => "Referral status is temporarily unavailable",
        ReferralOperation::Payout => "Referral payout setup is temporarily unavailable",
    };
    HumanErrorPresentation {
        title,
        detail: Some(
            "The ctx Pro service could not complete the request. Local ctx data is unchanged.",
        ),
        hint: "Try again after connectivity returns.",
        action: Some(retry_command),
    }
}

fn referral_invalid_response<'a>(
    operation: ReferralOperation,
    signals: TrustedErrorSignals,
    retry_command: &'a str,
) -> HumanErrorPresentation<'a> {
    let (title, detail) = match operation {
        ReferralOperation::Create => (
            "Referral creation returned an invalid response",
            "The referral creation result failed validation. No codename was accepted.",
        ),
        ReferralOperation::Status if signals.malformed_commercial_error => (
            "The referral service returned an invalid response",
            "The service error failed validation. No referral data was accepted.",
        ),
        ReferralOperation::Status => (
            "Referral status returned an invalid response",
            "The referral aggregate failed validation. No balances were shown.",
        ),
        ReferralOperation::Payout => (
            "Referral payout returned an invalid response",
            "The payout setup result failed validation. No onboarding link was accepted.",
        ),
    };
    HumanErrorPresentation {
        title,
        detail: Some(detail),
        hint: "Try again later. If the failure continues, contact ctx support.",
        action: Some(retry_command),
    }
}

#[cfg(test)]
mod human_error_tests;

#[cfg(test)]
mod tests {
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
                assert!(
                    styled.contains(&format!("\u{1b}[36m{action}")),
                    "{styled:?}"
                );
                assert_eq!(strip_ansi(&styled), plain);
                let maximum = context.content_width().unwrap();
                assert!(plain.lines().all(|line| {
                    line.width() <= maximum || line.trim_start().starts_with("ctx ")
                }));
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
            let document =
                human_actionable_error_document(&context(80, crate::ui::ColorMode::Never), &anyhow!(raw), retry)
                    .unwrap();
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✗ "), "{rendered}");
            assert!(normalized.contains(title), "{rendered}");
            assert!(normalized.contains(detail), "{rendered}");
            assert!(rendered.contains(&format!("Next\n  {action}\n")), "{rendered}");
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
        let pipe =
            RenderContext::for_test(crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr));
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
            "invalid_request: qualification helpers are unsupported on this platform",
            "helper_upgrade_required: private helper detail",
            "commercial_identity_conflict: bounded safe cause",
            "authentication_expired: WorkOS device authorization expired",
            "invalid_response: referral payout result is invalid",
            "protocol_mismatch: untrusted helper detail",
        ] {
            let error =
                human_result::<()>(Err(anyhow!(raw)), false, "ctx referral payout", &mut ui)
                    .unwrap_err();
            assert_eq!(error.to_string(), raw);
        }
    }

    #[test]
    fn unrelated_errors_keep_their_existing_contract() {
        let error = anyhow!("authentication_required: sign in");
        let context =
            RenderContext::for_test(crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr));
        assert!(human_actionable_error_document(&context, &error, "ctx pro").is_none());
    }
}

mod anonymous_trial;
pub(crate) mod artifact_delivery;
mod authorization;
mod client;
mod commercial_api;
mod commercial_config;
mod commercial_deletion;
mod commercial_lifecycle;
mod commercial_production_record;
mod core_worker_budget;
mod credential_vault;
pub(crate) mod diagnostic;
pub(crate) mod evidence_preview;
mod graph_key_deletion;
mod helper_command;
mod lifecycle;
mod local_deletion;
mod pending_materialization;
mod pricing;
mod referral;
mod render;
mod request_identity;
mod setup_validation;
#[cfg(ctx_pro_test_helper)]
mod test_control;
mod verified_executable;
mod workos_device;
use std::io;

use crate::ui::{hint, outcome, Action, Document, Hint, Outcome, OutcomeState, RenderContext, Ui};
pub(crate) use client::{
    blame, blame_boundary_error, blame_diagnostic, core_finalization_generation_lease,
    invalid_blame_request, preflight_core_materialization,
    reconstruct_core_finalization_generation_lease, release_core_finalization_generation_lease,
    selected_helper_artifact_sha256, stable_error_code, sync_core_materialization,
    validate_core_finalization_generation_lease, BlameResultFreshness,
    CoreMaterializationSyncOutcome, HostedBlameResult, RESOURCE_NOT_FOUND_DIAGNOSTIC,
};
#[cfg(test)]
pub(crate) use lifecycle::count_lifecycle_status_queries;
pub(crate) use lifecycle::{
    lifecycle_status_json, lifecycle_status_json_for_core, run_lifecycle, ProArgs,
};
pub(crate) use pricing::PRO_MONTHLY_PRICE_DISPLAY;
#[cfg(test)]
pub(crate) use referral::parse_referral_codename;
pub(crate) use referral::{run as run_referral, show_cta_once, ReferralArgs};
pub(crate) use render::{
    blame_result_json, print_blame_result, print_blame_result_with_evidence_preview,
};

use anyhow::{anyhow, Result};
use serde::Serialize;
pub(crate) const DEFAULT_BLAME_LIMIT: u32 = 20;

#[derive(Serialize)]
struct StableErrorOutput {
    error: &'static str,
    error_code: &'static str,
}

pub(crate) fn write_stable_error_json(
    output: &mut impl io::Write,
    error: &anyhow::Error,
) -> Result<bool> {
    let diagnostic = client::typed_blame_diagnostic(error).cloned();
    let Some(code) = diagnostic
        .as_ref()
        .map(|value| value.error_code)
        .or_else(|| stable_error_code(error))
    else {
        return Ok(false);
    };
    if let Some(diagnostic) = diagnostic {
        serde_json::to_writer(&mut *output, &diagnostic)?;
    } else {
        serde_json::to_writer(
            &mut *output,
            &StableErrorOutput {
                error: code,
                error_code: code,
            },
        )?;
    }
    writeln!(output)?;
    Ok(true)
}

pub(crate) fn write_blame_error_json(
    output: &mut impl io::Write,
    error: &anyhow::Error,
) -> Result<()> {
    let diagnostic = blame_diagnostic(error).unwrap_or_else(|| {
        diagnostic::BlameDiagnostic::for_stable_error_code("invalid_response")
            .expect("invalid_response has a trusted blame diagnostic")
    });
    serde_json::to_writer(&mut *output, &diagnostic)?;
    writeln!(output)?;
    Ok(())
}

pub(crate) fn actionable_error(error: anyhow::Error) -> anyhow::Error {
    // Preserve protocol-originated diagnostics as typed errors. Their Display
    // value is already the legacy stable code, while renderers can downcast to
    // the trusted structured contract.
    if client::typed_blame_diagnostic(&error).is_some() {
        return error;
    }
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
    let document = human_blame_diagnostic_document(ui.stderr_context(), &error)
        .or_else(|| human_actionable_error_document(ui.stderr_context(), &error, "ctx pro"));
    let Some(document) = document else {
        return Err(actionable_error(error));
    };
    ui.write_stderr(&document)?;
    Err(crate::dispatch::rendered_cli_error())
}

fn human_blame_diagnostic_document(
    context: &RenderContext,
    error: &anyhow::Error,
) -> Option<Document> {
    let diagnostic = blame_diagnostic(error)?;
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: diagnostic.message,
            detail: None,
        },
    );
    if let Some(action) = diagnostic.next_action {
        let command = action
            .argv
            .iter()
            .map(|argument| crate::transcript::shell_quote_arg(argument))
            .collect::<Vec<_>>()
            .join(" ");
        if !command.is_empty() {
            document.push_blank();
            document.append(hint(
                context,
                Hint { text: "Try:" },
                Some(Action { command: &command }),
            ));
        }
    }
    Some(document)
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
            detail: Some("No helper response was accepted."),
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
                "The checkout may still be open, but ctx stopped waiting after 30 minutes.",
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
        "referral_payout_country_required" => HumanErrorPresentation {
            title: "A payout country is required",
            detail: Some(
                "Choose the country where you will receive referral payouts, or provide its two-letter code.",
            ),
            hint: "Supply the country code for payout setup.",
            action: Some("ctx referral payout --country <CC>"),
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
            detail: Some("Index local agent history before retrying this command."),
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
mod tests;

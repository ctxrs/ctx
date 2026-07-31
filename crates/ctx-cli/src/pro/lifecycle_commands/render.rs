use std::io;

use anyhow::{bail, Result};
use serde_json::Value;

use super::{
    LocalProDataOutcome, ProArgs, ProCommand, ProManageArgs, ProManagePlan,
    UninstallDataDisposition, UNINSTALL_DATA_PROMPT,
};
use crate::{
    local_usage::UsageReport,
    pro::PRO_MONTHLY_PRICE_DISPLAY,
    ui::{
        fields, hint, outcome, section, Action, Document, Field, Hint, Outcome as UiOutcome,
        OutcomeState, RenderContext, Ui,
    },
};

pub(super) fn human_retry_command(args: &ProArgs) -> &'static str {
    match &args.command {
        Some(ProCommand::Manage(ProManageArgs { no_open: true, .. })) => "ctx pro manage --no-open",
        Some(ProCommand::Manage(_)) => "ctx pro manage",
        None | Some(ProCommand::Setup(_)) | Some(ProCommand::Uninstall(_)) => "ctx pro",
    }
}

pub(super) fn prompt_uninstall_data_disposition(
    input: &mut impl io::BufRead,
    ui: &mut Ui,
) -> Result<UninstallDataDisposition> {
    loop {
        ui.write_stderr(&uninstall_prompt(ui.stderr_context()))?;
        ui.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            bail!("cancelled: uninstall confirmation was not provided");
        }
        match answer.trim() {
            "" | "y" | "Y" | "yes" | "YES" => return Ok(UninstallDataDisposition::Delete),
            "n" | "N" | "no" | "NO" => return Ok(UninstallDataDisposition::Keep),
            _ => ui.write_stderr(&uninstall_answer_required(ui.stderr_context()))?,
        }
    }
}

pub(super) fn uninstall_prompt(context: &RenderContext) -> Document {
    outcome(
        context,
        UiOutcome {
            state: OutcomeState::Warning,
            title: UNINSTALL_DATA_PROMPT,
            detail: Some("Canonical ctx history is always preserved."),
        },
    )
}

pub(super) fn uninstall_answer_required(context: &RenderContext) -> Document {
    outcome(
        context,
        UiOutcome {
            state: OutcomeState::Warning,
            title: "Please answer y or n.",
            detail: None,
        },
    )
}

pub(super) fn setup(context: &RenderContext, account_state: &str) -> Document {
    let title = if account_state == "trial" {
        "ctx Pro trial is ready"
    } else {
        "ctx Pro is ready"
    };
    let product = if account_state == "trial" {
        "Free ctx Pro trial"
    } else {
        PRO_MONTHLY_PRICE_DISPLAY
    };
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Success,
            title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(section(
        "Pro",
        fields(
            context,
            &[
                Field::new("Product", product),
                Field::new("Access", access_state(account_state)),
                Field::new("Work graph", "Ready"),
            ],
        ),
    ));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Manage trial, account, or billing details.",
        },
        Some(Action {
            command: "ctx pro manage",
        }),
    ));
    document
}

pub(super) fn manage(
    context: &RenderContext,
    plan: &ProManagePlan,
    usage: &UsageReport,
    conversion: Option<&Value>,
    browser_opened: bool,
) -> Document {
    let product = if plan.access_state == "trial" {
        "Free ctx Pro trial"
    } else {
        PRO_MONTHLY_PRICE_DISPLAY
    };
    let graph = if plan.access_state == "locked" {
        "Preserved locally; access is locked"
    } else {
        "Available locally"
    };
    let field_values = [
        Field::new("Product", product),
        Field::new("Access", access_state(&plan.access_state)),
        Field::new("Work graph", graph),
        Field::new("Management link", &plan.portal_url),
    ];
    let mut document = outcome(
        context,
        UiOutcome {
            state: if plan.access_state == "locked" {
                OutcomeState::Warning
            } else {
                OutcomeState::Success
            },
            title: "ctx Pro account management is ready",
            detail: None,
        },
    );
    document.push_blank();
    document.append(section("Pro", fields(context, &field_values)));
    document.push_blank();
    document.append(local_usage(context, usage));
    if let Some(conversion) = conversion {
        document.push_blank();
        document.append(conversion_facts(context, conversion));
    }
    document.push_blank();
    let next = if browser_opened {
        None
    } else {
        Some(Action {
            command: &plan.portal_url,
        })
    };
    document.append(hint(
        context,
        Hint {
            text: if browser_opened {
                "Finish account or billing changes in the browser."
            } else {
                "Open the management link to change billing or restore access."
            },
        },
        next,
    ));
    document
}

fn local_usage(context: &RenderContext, report: &UsageReport) -> Document {
    let state = match report.state {
        "ready" => "Ready",
        "empty" => "Enabled; no aggregate facts recorded yet",
        "disabled" => "Disabled",
        "error" => "Unavailable",
        state => state,
    };
    let retention = format!("{} UTC days", report.retention_days);
    let mut values = vec![
        Field::new("Status", state),
        Field::new("Retention", &retention),
    ];
    let error = report
        .error
        .as_ref()
        .map(|error| format!("{}: {}", error.code, error.message));
    if let Some(error) = error.as_deref() {
        values.push(Field::new("Issue", error));
    }

    let definitions = report.definitions.as_deref().unwrap_or_default();
    let definition_count = definitions.len().to_string();
    if !definitions.is_empty() {
        values.push(Field::new("Measurement definitions", &definition_count));
    }
    let mut document = section("Local usage", fields(context, &values));
    for definition in definitions {
        let heading = format!(
            "Measured local facts · definition {}",
            definition.definition_version
        );
        let period = format!(
            "{} active UTC {} · {} through {}",
            definition.active_days,
            if definition.active_days == 1 {
                "day"
            } else {
                "days"
            },
            definition.first_day_utc,
            definition.last_day_utc
        );
        let versions = definition.ctx_versions.join(", ");
        let calls = format!(
            "{} total · {} succeeded · {} failed",
            definition.summary.calls,
            definition.summary.successful_calls,
            definition.summary.failed_calls
        );
        let result_sets = format!(
            "{} nonempty · {} empty",
            definition.summary.result_bearing_calls, definition.summary.empty_calls
        );
        let unclassified = format!("{} calls", definition.summary.not_applicable_calls);
        let results = format!(
            "{} results · {} unique blame citations",
            definition.summary.result_count, definition.summary.citation_count
        );
        let output = format!("{} bytes", definition.summary.delivered_output_bytes);
        let covered = format!("{} bytes", definition.summary.delivered_context_bytes);
        let matched = format!(
            "{} bytes",
            definition.summary.matched_normalized_session_bytes
        );
        let coverage = format!(
            "{} complete · {} unavailable",
            definition.summary.complete_context_eligible_calls,
            definition.summary.unavailable_context_eligible_calls
        );
        let mut facts = vec![
            Field::new("Period", &period),
            Field::new("ctx versions", &versions),
            Field::new("Calls", &calls),
            Field::new("Classified result sets", &result_sets),
            Field::new("No result-set classification", &unclassified),
            Field::new("Results", &results),
            Field::new("Delivered output", &output),
            Field::new("Covered context", &covered),
            Field::new("Matched history", &matched),
            Field::new("Search coverage", &coverage),
        ];
        let blame = &definition.summary.pro_blame;
        let blame_outcomes = (blame.requests > 0).then(|| {
            format!(
                "{} produced-attribution · {} possible-only · {} none · {} error",
                blame.produced_attribution_requests,
                blame.possible_only_requests,
                blame.none_requests,
                blame.error_requests
            )
        });
        if let Some(blame_outcomes) = blame_outcomes.as_deref() {
            facts.push(Field::new("Blame outcomes", blame_outcomes));
        }
        document.push_blank();
        document.append(section(&heading, fields(context, &facts)));
    }
    if let Some(estimates) = &report.estimates {
        let tokens = estimates.approximate_context_tokens;
        let covered = format!("{} bytes", tokens.delivered_context_bytes);
        let range = format!(
            "{} low · {} central · {} high",
            tokens.token_equivalents.low,
            tokens.token_equivalents.central,
            tokens.token_equivalents.high
        );
        document.push_blank();
        document.append(section(
            "Approximate token-equivalents",
            fields(
                context,
                &[
                    Field::new("Covered context", &covered),
                    Field::new("Range", &range),
                    Field::new("Coefficient", tokens.coefficient_version),
                ],
            ),
        ));

        let reduction = estimates.estimated_context_reduction;
        let bytes = format!(
            "{} baseline · {} observed · {} estimated reduction",
            reduction.comparison_baseline_bytes,
            reduction.observed_delivered_context_bytes,
            reduction.estimated_avoided_context_bytes
        );
        let token_range = format!(
            "{} low · {} central · {} high",
            reduction.approximate_token_equivalents.low,
            reduction.approximate_token_equivalents.central,
            reduction.approximate_token_equivalents.high
        );
        let coverage = format!(
            "{} covered · {} unavailable",
            reduction.covered_calls, reduction.unavailable_calls
        );
        document.push_blank();
        document.append(section(
            "Estimated context reduction",
            fields(
                context,
                &[
                    Field::new("Bytes", &bytes),
                    Field::new("Token-equivalents", &token_range),
                    Field::new("Coverage", &coverage),
                    Field::new("Model", reduction.estimate_model_version),
                    Field::new("Coefficient", reduction.coefficient_version),
                ],
            ),
        ));
    }
    document
}

fn conversion_facts(context: &RenderContext, action: &Value) -> Document {
    let command = action
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("ctx pro manage");
    let (label, value) = if action.get("kind").and_then(Value::as_str) == Some("pro_restore_access")
    {
        (
            "Recovery",
            "Restore ctx Pro access; the local graph is preserved.".to_owned(),
        )
    } else {
        let price = action
            .get("price")
            .and_then(Value::as_str)
            .unwrap_or(PRO_MONTHLY_PRICE_DISPLAY);
        ("Offer", format!("Continue with ctx Pro for {price}."))
    };
    section(
        "Access",
        fields(
            context,
            &[Field::new(label, &value), Field::new("Command", command)],
        ),
    )
}

pub(super) fn browser_notice(
    context: &RenderContext,
    browser_opened: bool,
    destination: &str,
) -> Document {
    let title = if browser_opened {
        format!("Browser open requested for {destination}.")
    } else {
        format!("A browser could not be opened for {destination}.")
    };
    outcome(
        context,
        UiOutcome {
            state: if browser_opened {
                OutcomeState::Neutral
            } else {
                OutcomeState::Warning
            },
            title: &title,
            detail: (!browser_opened).then_some("Use the link in the command output."),
        },
    )
}

pub(super) fn uninstall(
    context: &RenderContext,
    helper_removed: bool,
    data_outcome: LocalProDataOutcome,
) -> Document {
    let (title, data) = match data_outcome {
        LocalProDataOutcome::Deleted => ("ctx Pro was removed", "Deleted"),
        LocalProDataOutcome::Preserved => ("ctx Pro was removed", "Preserved locally"),
        LocalProDataOutcome::Absent if helper_removed => {
            ("ctx Pro was removed", "No local Pro data found")
        }
        LocalProDataOutcome::Absent => (
            "No ctx Pro installation or local Pro data was found",
            "No local Pro data found",
        ),
    };
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Success,
            title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(section(
        "Local data",
        fields(
            context,
            &[
                Field::new("Pro graph", data),
                Field::new("Canonical ctx history", "Preserved"),
            ],
        ),
    ));
    document.push_blank();
    match data_outcome {
        LocalProDataOutcome::Deleted => document.append(hint(
            context,
            Hint {
                text: "Set up Pro again to rebuild local Pro data.",
            },
            Some(Action { command: "ctx pro" }),
        )),
        LocalProDataOutcome::Preserved => document.append(hint(
            context,
            Hint {
                text: "Set up Pro again to restore the preserved graph.",
            },
            Some(Action { command: "ctx pro" }),
        )),
        LocalProDataOutcome::Absent => document.append(hint(
            context,
            Hint {
                text: "No further action is required.",
            },
            None,
        )),
    }
    document
}

fn access_state(state: &str) -> &'static str {
    match state {
        "trial" => "Trial active",
        "active" => "Active",
        "canceling_paid" => "Active; cancellation scheduled",
        "offline_grace" => "Offline grace",
        "locked" => "Locked",
        _ => "Unavailable",
    }
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let maximum = context.content_width().unwrap_or(1);
        let rendered = document.render_plain();
        for line in rendered.lines() {
            if line.width() <= maximum {
                continue;
            }
            let preserves_copyable_atom = line.trim_start().starts_with("ctx ")
                || line.split_whitespace().any(|atom| {
                    atom.contains("://")
                        || atom.starts_with("--")
                        || uuid::Uuid::parse_str(atom).is_ok()
                });
            assert!(
                preserves_copyable_atom,
                "{line:?} exceeded {maximum} columns"
            );
        }
    }

    #[test]
    fn setup_is_outcome_first_and_has_one_next_command() {
        assert_eq!(
            setup(&context(80), "trial").render_plain(),
            concat!(
                "✓ ctx Pro trial is ready\n\n",
                "Pro\n",
                "Product     Free ctx Pro trial\n",
                "Access      Trial active\n",
                "Work graph  Ready\n\n",
                "Hint: Manage trial, account, or billing details.\n\n",
                "Next\n",
                "  ctx pro manage\n",
            )
        );
    }

    #[test]
    fn manage_sanitizes_the_link_and_uninstall_states_are_truthful() {
        let plan = ProManagePlan {
            portal_url: "https://billing.example.test/session\u{1b}[2J".to_owned(),
            access_state: "locked".to_owned(),
            refresh_after_unix: None,
            access_deadline_unix: None,
            grace_deadline_unix: None,
        };
        let usage = UsageReport::config_error();
        let conversion = crate::local_usage::pro_conversion_action(Some("locked"));
        let rendered =
            manage(&context(120), &plan, &usage, conversion.as_ref(), false).render_plain();
        assert!(rendered.starts_with("! ctx Pro account management is ready\n"));
        assert!(rendered.contains("Management link"));
        assert!(rendered.contains("\\x1b"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("Preserved locally; access is locked"));
        assert!(rendered.contains("Local usage"));
        assert!(rendered.contains("local_usage_config_unavailable"));
        assert!(rendered.contains("Restore ctx Pro access; the local graph is preserved."));
        assert!(rendered.contains("ctx pro manage"));
        assert!(rendered.contains("\nNext\n  https://billing.example.test/session\\x1b[2J\n"));

        let opened = manage(&context(120), &plan, &usage, conversion.as_ref(), true).render_plain();
        assert!(opened.contains("Finish account or billing changes in the browser."));
        assert!(!opened.contains("\nNext\n"));

        assert!(uninstall(&context(80), true, LocalProDataOutcome::Deleted)
            .render_plain()
            .contains("Pro graph              Deleted"));
        assert!(
            uninstall(&context(80), true, LocalProDataOutcome::Preserved)
                .render_plain()
                .contains("Pro graph              Preserved locally")
        );
        assert!(uninstall(&context(80), false, LocalProDataOutcome::Absent)
            .render_plain()
            .contains("No further action is required."));
    }

    #[test]
    fn lifecycle_renderers_fit_supported_widths() {
        let plan = ProManagePlan {
            portal_url: "https://billing.example.test/session".to_owned(),
            access_state: "trial".to_owned(),
            refresh_after_unix: Some(100),
            access_deadline_unix: Some(200),
            grace_deadline_unix: None,
        };
        let usage = UsageReport::config_error();
        let conversion = crate::local_usage::pro_conversion_action(Some("trial"));
        for width in [32, 48, 80, 120] {
            let context = context(width);
            for document in [
                setup(&context, "trial"),
                manage(&context, &plan, &usage, conversion.as_ref(), false),
                uninstall(&context, true, LocalProDataOutcome::Deleted),
                uninstall_prompt(&context),
            ] {
                assert_fits(&document, &context);
            }
        }
    }
}

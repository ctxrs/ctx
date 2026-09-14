use anyhow::{anyhow, Result};
use ctx_agent_application::{
    integrations::slash_commands::{
        self as application, SlashCommandRemoveApplicationRequest,
        SlashCommandStatusApplicationRequest,
    },
    ProductIdentity,
};
use ctx_agent_integrations::slash_commands::{
    SlashCommandInstallStatus, SlashCommandRemoveResult, SlashCommandScope,
    SlashCommandStatusResult, COMMAND_NAME,
};
use serde_json::{json, Value};

use crate::{
    analytics::IntegrationTelemetry,
    ui::{
        diagnostic, empty_state, fields, hint, outcome, section, table, Action, Diagnostic,
        DiagnosticLevel, Document, EmptyState, Field, Hint, Outcome, OutcomeState, RenderContext,
        Table, Ui,
    },
};

use super::{PathContext, SlashCommandAgentArg, SlashCommandRemoveArgs, SlashCommandStatusArgs};

pub(crate) fn run_status(
    args: SlashCommandStatusArgs,
    context: &PathContext,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.format.is_json();
    let outcome = application::status(
        SlashCommandStatusApplicationRequest {
            agents: integration_agents(&args.agent),
            all_agents: args.all_agents,
            project: args.project,
        },
        context,
        identity,
    );
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    let receipt = outcome.receipt;
    if json_output {
        write_json(
            ui,
            json!({
                "integration": "slash-commands",
                "command": COMMAND_NAME,
                "scope": if receipt.request.project { "project" } else { "global" },
                "results": receipt.results.iter().map(status_result_json).collect::<Vec<_>>(),
            }),
        )?;
    } else {
        ui.write_stdout(&render_status_results(
            ui.stdout_context(),
            &receipt.results,
            outcome.recovery_command.as_deref(),
        ))?;
        if let Some(diagnostics) = render_status_failures(ui.stderr_context(), &receipt.results) {
            ui.write_stderr(&diagnostics)?;
        }
    }
    finish("inspect", receipt.failed, json_output)
}

pub(crate) fn run_remove(
    args: SlashCommandRemoveArgs,
    context: &PathContext,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.format.is_json();
    let outcome = application::remove(
        SlashCommandRemoveApplicationRequest {
            agents: integration_agents(&args.agent),
            all_agents: args.all_agents,
            project: args.project,
            force: args.force,
        },
        context,
    );
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    let receipt = outcome.receipt;
    if json_output {
        write_json(
            ui,
            json!({
                "integration": "slash-commands",
                "command": COMMAND_NAME,
                "scope": if receipt.project { "project" } else { "global" },
                "results": receipt.results.iter().map(remove_result_json).collect::<Vec<_>>(),
            }),
        )?;
    } else {
        ui.write_stdout(&render_remove_results(
            ui.stdout_context(),
            &receipt.results,
        ))?;
        if let Some(diagnostics) =
            render_remove_failures(ui.stderr_context(), identity, &receipt.results)
        {
            ui.write_stderr(&diagnostics)?;
        }
    }
    finish("remove", receipt.failed, json_output)
}

fn integration_agents(
    args: &[SlashCommandAgentArg],
) -> Vec<ctx_agent_integrations::slash_commands::SlashCommandAgent> {
    args.iter()
        .copied()
        .map(SlashCommandAgentArg::integration)
        .collect()
}

fn write_json(ui: &mut Ui, value: Value) -> Result<()> {
    let mut output = serde_json::to_string_pretty(&value)?;
    output.push('\n');
    Ok(ui.write_stdout_bytes(output.as_bytes())?)
}

fn finish(action: &str, failed: usize, json_output: bool) -> Result<()> {
    if failed == 0 {
        return Ok(());
    }
    if !json_output {
        return Err(crate::rendered_cli_error());
    }
    Err(anyhow!(
        "failed to {action} slash commands for {failed} target(s)"
    ))
}

fn status_result_json(result: &SlashCommandStatusResult) -> Value {
    json!({
        "agent": result.agent.id(),
        "agent_display_name": result.agent.display_name(),
        "scope": result.scope.map(SlashCommandScope::as_str),
        "path": result.path,
        "legacy_path": result.legacy_path,
        "success": result.success,
        "status": result.status.as_str(),
        "error": result.error,
        "note": result.note,
    })
}

fn remove_result_json(result: &SlashCommandRemoveResult) -> Value {
    json!({
        "agent": result.agent.id(),
        "agent_display_name": result.agent.display_name(),
        "scope": result.scope.map(SlashCommandScope::as_str),
        "path": result.path,
        "legacy_path": result.legacy_path,
        "success": result.success,
        "previous_status": result.previous_status.as_str(),
        "status": result.status.as_str(),
        "already_absent": result.already_absent,
        "modified": result.modified,
        "current_removed": result.current_removed,
        "legacy_removed": result.legacy_removed,
        "metadata_removed": result.metadata_removed,
        "error": result.error,
        "note": result.note,
    })
}

fn render_status_results(
    context: &RenderContext,
    results: &[SlashCommandStatusResult],
    recovery_command: Option<&str>,
) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No slash-command targets detected",
                detail: "Select a file-based agent explicitly to inspect it.",
                action: Some(Action {
                    command: "ctx integrations status slash-command --all-agents",
                }),
            },
        );
    }
    let failed = results.iter().filter(|result| !result.success).count();
    let needs_action = results
        .iter()
        .filter(|result| {
            matches!(
                result.status,
                SlashCommandInstallStatus::Missing
                    | SlashCommandInstallStatus::Stale
                    | SlashCommandInstallStatus::Modified
            )
        })
        .count();
    let title = if failed > 0 {
        "Slash-command status needs attention".to_owned()
    } else if needs_action == 0 {
        "Slash-command integrations inspected".to_owned()
    } else {
        format!("{needs_action} slash-command target(s) need attention")
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if failed > 0 || needs_action > 0 {
                OutcomeState::Warning
            } else {
                OutcomeState::Success
            },
            title: &title,
            detail: None,
        },
    );
    append_status_table(context, &mut document, results);
    if let Some(command) = recovery_command {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Install or repair the selected slash-command targets.",
            },
            Some(Action { command }),
        ));
    }
    document
}

fn append_status_table(
    context: &RenderContext,
    document: &mut Document,
    results: &[SlashCommandStatusResult],
) {
    let mut targets = Table::new(["Agent", "Status", "Location"]);
    for result in results {
        targets.push_row([
            result.agent.display_name().to_owned(),
            result.status.as_str().replace('_', "-"),
            location(result.scope, result.path.as_deref()),
        ]);
    }
    document.push_blank();
    document.append(section("Targets", table(context, &targets)));
    append_notes(
        context,
        document,
        results.iter().filter_map(|result| {
            result
                .note
                .as_deref()
                .map(|note| (result.agent.display_name(), note))
        }),
    );
}

fn render_remove_results(
    context: &RenderContext,
    results: &[SlashCommandRemoveResult],
) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No slash-command targets detected",
                detail: "Select a file-based agent explicitly to remove it.",
                action: Some(Action {
                    command: "ctx integrations remove slash-command --all-agents",
                }),
            },
        );
    }
    let failed = results.iter().filter(|result| !result.success).count();
    let removed = results.iter().filter(|result| result.modified).count();
    let title = if failed > 0 {
        format!("{failed} slash-command target(s) need attention")
    } else if removed == 0 {
        "Slash-command integrations are already absent".to_owned()
    } else {
        format!("{removed} slash-command target(s) removed")
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if failed > 0 {
                OutcomeState::Warning
            } else {
                OutcomeState::Success
            },
            title: &title,
            detail: None,
        },
    );
    let mut targets = Table::new(["Agent", "Status", "Location"]);
    for result in results {
        let status = if !result.success {
            if result.modified {
                removed_parts(result).map_or_else(
                    || "partially removed".to_owned(),
                    |parts| format!("partial ({parts})"),
                )
            } else {
                "preserved".to_owned()
            }
        } else if result.modified {
            "removed".to_owned()
        } else if matches!(
            result.status,
            SlashCommandInstallStatus::SkillOnly | SlashCommandInstallStatus::ManualOnly
        ) {
            "not managed here".to_owned()
        } else {
            "absent".to_owned()
        };
        targets.push_row([
            result.agent.display_name().to_owned(),
            status,
            location(result.scope, result.path.as_deref()),
        ]);
    }
    document.push_blank();
    document.append(section("Targets", table(context, &targets)));
    append_notes(
        context,
        &mut document,
        results.iter().filter_map(|result| {
            result
                .note
                .as_deref()
                .map(|note| (result.agent.display_name(), note))
        }),
    );
    document
}

fn render_status_failures(
    context: &RenderContext,
    results: &[SlashCommandStatusResult],
) -> Option<Document> {
    render_failures(
        context,
        results
            .iter()
            .filter(|result| !result.success)
            .map(|result| {
                (
                    format!("{} could not be inspected", result.agent.display_name()),
                    result.error.as_deref(),
                    None,
                )
            }),
    )
}

fn render_remove_failures(
    context: &RenderContext,
    identity: ProductIdentity<'_>,
    results: &[SlashCommandRemoveResult],
) -> Option<Document> {
    let failures = results
        .iter()
        .filter(|result| !result.success)
        .map(|result| {
            let summary = if result.modified {
                removed_parts(result).map_or_else(
                    || {
                        format!(
                            "{} removal was incomplete after managed state changed",
                            result.agent.display_name()
                        )
                    },
                    |parts| {
                        format!(
                            "{} removal was incomplete after removing {parts}",
                            result.agent.display_name()
                        )
                    },
                )
            } else {
                format!("{} was not removed", result.agent.display_name())
            };
            (
                summary,
                result.error.as_deref(),
                application::force_remove_command(identity, result),
            )
        })
        .collect::<Vec<_>>();
    render_failures(
        context,
        failures
            .iter()
            .map(|(summary, detail, command)| (summary.clone(), *detail, command.as_deref())),
    )
}

fn removed_parts(result: &SlashCommandRemoveResult) -> Option<String> {
    let mut parts = Vec::new();
    if result.current_removed {
        parts.push("command");
    }
    if result.legacy_removed {
        parts.push("legacy command");
    }
    if result.metadata_removed {
        parts.push("metadata");
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn render_failures<'a>(
    context: &RenderContext,
    failures: impl IntoIterator<Item = (String, Option<&'a str>, Option<&'a str>)>,
) -> Option<Document> {
    let mut document = Document::new();
    for (summary, detail, command) in failures {
        if !document.is_empty() {
            document.push_blank();
        }
        document.append(diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &summary,
                detail,
                fields: &[],
                action: command.map(|command| Action { command }),
            },
        ));
    }
    (!document.is_empty()).then_some(document)
}

fn append_notes<'a>(
    context: &RenderContext,
    document: &mut Document,
    notes: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    let notes = notes
        .into_iter()
        .map(|(agent, note)| Field::new(agent, note))
        .collect::<Vec<_>>();
    if notes.is_empty() {
        return;
    }
    document.push_blank();
    document.append(section("Notes", fields(context, &notes)));
}

fn location(scope: Option<SlashCommandScope>, path: Option<&std::path::Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_else(|| match scope {
            Some(scope) => scope.as_str().to_owned(),
            None => "-".to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ui::{StreamKind, TestContext};
    use ctx_agent_integrations::slash_commands::SlashCommandAgent;

    const PRODUCT: ProductIdentity<'static> = ProductIdentity {
        name: "ctx",
        version: "1.0.0-test",
    };

    fn partial_failure() -> SlashCommandRemoveResult {
        SlashCommandRemoveResult {
            agent: SlashCommandAgent::OpenCode,
            scope: Some(SlashCommandScope::Project),
            path: Some(PathBuf::from("/tmp/project/.opencode/commands/ctx.md")),
            legacy_path: None,
            success: false,
            previous_status: SlashCommandInstallStatus::Current,
            status: SlashCommandInstallStatus::Modified,
            already_absent: false,
            modified: true,
            current_removed: true,
            legacy_removed: true,
            metadata_removed: true,
            force_required: false,
            error: Some("target changed during final inspection".to_owned()),
            note: None,
        }
    }

    #[test]
    fn partial_removal_reports_removed_components_and_incomplete_state() {
        let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
        let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
        let result = partial_failure();

        let summary =
            render_remove_results(&stdout_context, std::slice::from_ref(&result)).render_plain();
        assert!(summary.contains("partial (command, legacy command, metadata)"));
        assert!(!summary.contains("preserved"));

        let diagnostic = render_remove_failures(&stderr_context, PRODUCT, &[result])
            .unwrap()
            .render_plain();
        assert!(diagnostic
            .contains("removal was incomplete after removing command, legacy command, metadata"));
        assert!(!diagnostic.contains("was not removed"));
    }
}

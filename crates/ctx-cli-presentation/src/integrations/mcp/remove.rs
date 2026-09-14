use anyhow::{anyhow, Result};
use ctx_agent_application::{integrations::mcp as application, ProductIdentity};
use ctx_agent_integrations::mcp_config::{McpRemoveRequest, McpRemoveResult};
use serde_json::{json, Value};

use crate::{
    analytics::IntegrationTelemetry,
    ui::{
        diagnostic, empty_state, fields, outcome, section, Action, Diagnostic, DiagnosticLevel,
        Document, EmptyState, Field, Outcome, OutcomeState, RenderContext, Ui,
    },
};

use super::{
    format::{self, ConfigStatus},
    McpPathContext, McpRemoveArgs,
};

pub(super) fn run(
    args: McpRemoveArgs,
    context: &McpPathContext,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let outcome = application::remove(
        McpRemoveRequest {
            agents: args.target.agent.clone(),
            all_agents: args.target.all_agents,
            project: args.target.project,
            force: args.force,
        },
        context,
    );
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    let receipt = outcome.receipt;

    if args.format.is_json() {
        let command = format::server_command();
        let output = json!({
            "integration": "mcp",
            "server": {
                "name": identity.name,
                "command": command.executable(),
                "args": command.args(),
            },
            "scope": if receipt.project { "project" } else { "global" },
            "results": receipt.results.iter().map(mcp_remove_result_json).collect::<Vec<_>>(),
        });
        ui.write_stdout_bytes(format!("{output}\n").as_bytes())?;
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

    if receipt.failed == 0 {
        return Ok(());
    }
    if !args.format.is_json() {
        return Err(crate::rendered_cli_error());
    }
    Err(anyhow!(
        "failed to remove MCP integration for {} target(s)",
        receipt.failed
    ))
}

fn mcp_remove_result_json(result: &McpRemoveResult) -> Value {
    json!({
        "agent": result.target.agent.id(),
        "agent_display_name": result.target.agent.display_name(),
        "scope": result.target.scope.as_str(),
        "path": result.target.path,
        "detected": result.target.detected,
        "supported": result.target.unsupported_reason.is_none(),
        "success": result.success,
        "previous_status": result.previous_status.as_str(),
        "status": result.status.as_str(),
        "already_absent": result.already_absent,
        "modified": result.modified,
        "error": result.error,
    })
}

fn render_remove_results(context: &RenderContext, results: &[McpRemoveResult]) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No MCP-capable coding agents detected",
                detail: "Select a coding agent explicitly or remove every supported target.",
                action: Some(Action {
                    command: "ctx integrations remove mcp --all-agents",
                }),
            },
        );
    }

    let all_absent = results.iter().all(|result| result.already_absent);
    let all_success = results.iter().all(|result| result.success);
    let any_modified = results.iter().any(|result| result.modified);
    let title = if all_absent {
        "ctx MCP integration is already absent"
    } else if all_success && any_modified {
        "ctx MCP integration removed"
    } else {
        "ctx MCP integration needs attention"
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if all_success {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title,
            detail: None,
        },
    );

    let rows = results
        .iter()
        .map(|result| {
            let status = if result.already_absent {
                "absent"
            } else if result.modified {
                "removed"
            } else if result.success {
                "ok"
            } else {
                "skipped"
            };
            (status, mcp_remove_target_detail(result))
        })
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    document
}

fn render_remove_failures(
    context: &RenderContext,
    identity: ProductIdentity<'_>,
    results: &[McpRemoveResult],
) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| !result.success) {
        let summary = format!(
            "{} MCP configuration was not changed",
            result.target.agent.display_name()
        );
        let command = application::force_remove_command(identity, &result.target);
        if !document.is_empty() {
            document.push_blank();
        }
        document.append(diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &summary,
                detail: result.error.as_deref(),
                fields: &[],
                action: (result.status == ConfigStatus::Conflict)
                    .then_some(Action { command: &command }),
            },
        ));
    }
    (!document.is_empty()).then_some(document)
}

fn mcp_remove_target_detail(result: &McpRemoveResult) -> String {
    let mut detail = result.target.agent.display_name().to_owned();
    if let Some(path) = &result.target.path {
        detail.push_str(" -> ");
        detail.push_str(&path.display().to_string());
    }
    detail
}

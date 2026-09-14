use anyhow::{anyhow, Result};
use clap::{Args, ValueEnum};
use ctx_agent_application::{
    integrations::plugin::{self as application, PluginApplicationRequest},
    ProductIdentity,
};
use ctx_agent_integrations::plugin::{PluginAgent, PluginOperation, PluginReceipt, PluginResult};
use serde_json::{json, Value};

use crate::{
    analytics::{count_bucket, IntegrationScope, IntegrationTelemetry, TargetSelection},
    output::JsonOutputFormat,
    ui::{
        diagnostic, empty_state, fields, outcome, section, table, Diagnostic, DiagnosticLevel,
        Document, EmptyState, Field, Outcome, OutcomeState, RenderContext, Table, Ui,
    },
};

pub(crate) use ctx_agent_integrations::plugin::PluginContext;

#[derive(Debug, Args)]
pub(crate) struct PluginArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<PluginAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Use project scope when the host plugin manager supports it"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

pub(crate) type PluginInstallArgs = PluginArgs;
pub(crate) type PluginStatusArgs = PluginArgs;
pub(crate) type PluginRemoveArgs = PluginArgs;

impl PluginArgs {
    /// Shared accessor for the install, status, and remove dispatch variants.
    pub(crate) fn json_output(&self) -> bool {
        self.format.is_json()
    }

    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        telemetry.scope = Some(if self.project {
            IntegrationScope::Project
        } else {
            IntegrationScope::Global
        });
        telemetry.selection = Some(if self.all_agents {
            TargetSelection::All
        } else if self.agent.is_empty() {
            TargetSelection::Detected
        } else {
            TargetSelection::Explicit
        });
        telemetry.target_agents = Some(count_bucket(if self.all_agents {
            PluginAgent::ALL.len() as u64
        } else {
            self.agent.len() as u64
        }));
    }

    fn application_request(&self) -> PluginApplicationRequest {
        PluginApplicationRequest {
            agents: self
                .agent
                .iter()
                .copied()
                .map(PluginAgentArg::integration)
                .collect(),
            all_agents: self.all_agents,
            project: self.project,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PluginAgentArg {
    Codex,
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    Cursor,
}

impl PluginAgentArg {
    const fn integration(self) -> PluginAgent {
        match self {
            Self::Codex => PluginAgent::Codex,
            Self::ClaudeCode => PluginAgent::ClaudeCode,
            Self::Cursor => PluginAgent::Cursor,
        }
    }
}

pub(crate) fn run_install(
    args: PluginInstallArgs,
    context: &PluginContext,
    _identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.json_output();
    let outcome = application::install(args.application_request(), context);
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    present(outcome.receipt, json_output, ui)
}

pub(crate) fn run_status(
    args: PluginStatusArgs,
    context: &PluginContext,
    _identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.json_output();
    let outcome = application::status(args.application_request(), context);
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    present(outcome.receipt, json_output, ui)
}

pub(crate) fn run_remove(
    args: PluginRemoveArgs,
    context: &PluginContext,
    _identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.json_output();
    let outcome = application::remove(args.application_request(), context);
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    present(outcome.receipt, json_output, ui)
}

fn present(receipt: PluginReceipt, json_output: bool, ui: &mut Ui) -> Result<()> {
    if json_output {
        let mut encoded = serde_json::to_string_pretty(&receipt_json(&receipt))?;
        encoded.push('\n');
        ui.write_stdout_bytes(encoded.as_bytes())?;
    } else {
        ui.write_stdout(&render_receipt(ui.stdout_context(), &receipt))?;
        if let Some(diagnostics) = render_failures(ui.stderr_context(), &receipt.results) {
            ui.write_stderr(&diagnostics)?;
        }
    }

    let failures = if receipt.operation == PluginOperation::Status {
        receipt.operational_failures
    } else {
        receipt.failed
    };
    if failures == 0 {
        return Ok(());
    }
    if !json_output {
        return Err(crate::rendered_cli_error());
    }
    Err(anyhow!(
        "plugin {} needs attention for {failures} target(s)",
        receipt.operation.as_str()
    ))
}

pub(crate) fn receipt_json(receipt: &PluginReceipt) -> Value {
    json!({
        "integration": "plugin",
        "scope": receipt.scope.as_str(),
        "results": receipt.results.iter().map(result_json).collect::<Vec<_>>(),
    })
}

fn result_json(result: &PluginResult) -> Value {
    json!({
        "agent": result.agent.id(),
        "agent_display_name": result.agent.display_name(),
        "scope": result.scope.as_str(),
        "capability": result.capability.as_str(),
        "detected": result.detected,
        "supported": result.supported,
        "marketplace_status": result.marketplace_status.as_str(),
        "previous_status": result.previous_status.as_str(),
        "status": result.status.as_str(),
        "action": result.action.as_str(),
        "installed_version": result.installed_version,
        "success": result.success,
        "modified": result.modified,
        "instructions": result.instructions,
        "error": result.error,
    })
}

fn render_receipt(context: &RenderContext, receipt: &PluginReceipt) -> Document {
    if receipt.results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No plugin-capable coding agents detected",
                detail: "Select Codex, Claude Code, or Cursor explicitly, or target all agents.",
                action: None,
            },
        );
    }
    let needs_attention = receipt.results.iter().any(|result| !result.success);
    let title = if needs_attention {
        "ctx plugin integration needs attention"
    } else if receipt.modified > 0 {
        "ctx plugin integration updated"
    } else {
        "ctx plugin integration checked"
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if needs_attention {
                OutcomeState::Warning
            } else {
                OutcomeState::Success
            },
            title,
            detail: None,
        },
    );
    let mut targets = Table::new(["Agent", "Capability", "Status", "Action"]);
    for result in &receipt.results {
        targets.push_row([
            result.agent.display_name().to_owned(),
            result.capability.as_str().to_owned(),
            result.status.as_str().to_owned(),
            result.action.as_str().to_owned(),
        ]);
    }
    document.push_blank();
    document.append(section("Targets", table(context, &targets)));

    let instructions = receipt
        .results
        .iter()
        .filter_map(|result| {
            result
                .instructions
                .as_deref()
                .map(|instructions| Field::new(result.agent.display_name(), instructions))
        })
        .collect::<Vec<_>>();
    if !instructions.is_empty() {
        document.push_blank();
        document.append(section("Manual setup", fields(context, &instructions)));
    }
    document
}

fn render_failures(context: &RenderContext, results: &[PluginResult]) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| result.error.is_some()) {
        let summary = if result.modified {
            format!(
                "{} plugin integration changed, but the operation did not complete",
                result.agent.display_name()
            )
        } else {
            format!(
                "{} plugin integration was not changed",
                result.agent.display_name()
            )
        };
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
                action: None,
            },
        ));
    }
    (!document.is_empty()).then_some(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{StreamKind, TestContext};
    use ctx_agent_integrations::plugin::{
        PluginCapability, PluginInstallStatus, PluginMarketplaceStatus, PluginResultAction,
        PluginScope,
    };

    fn failed_result(modified: bool) -> PluginResult {
        PluginResult {
            agent: PluginAgent::Codex,
            scope: PluginScope::Global,
            capability: PluginCapability::Automatic,
            detected: true,
            supported: true,
            marketplace_status: PluginMarketplaceStatus::Present,
            previous_status: PluginInstallStatus::Missing,
            status: PluginInstallStatus::Error,
            action: PluginResultAction::Failed,
            installed_version: None,
            success: false,
            modified,
            instructions: None,
            error: Some("manager reconciliation failed".to_owned()),
            diagnostic: None,
            reconciliation_diagnostic: None,
        }
    }

    #[test]
    fn failure_summary_distinguishes_partial_mutation_from_no_change() {
        let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));

        let partial = render_failures(&context, &[failed_result(true)])
            .unwrap()
            .render_plain();
        assert!(partial.contains("integration changed, but the operation did not complete"));
        assert!(!partial.contains("was not changed"));

        let unchanged = render_failures(&context, &[failed_result(false)])
            .unwrap()
            .render_plain();
        assert!(unchanged.contains("plugin integration was not changed"));
    }
}

use anyhow::{anyhow, Result};
use clap::{Args, ValueEnum};
use ctx_agent_application::{
    integrations::slash_commands::{self as application, SlashCommandInstallApplicationRequest},
    ProductIdentity,
};
use ctx_agent_integrations::slash_commands::{
    SlashCommandAgent, SlashCommandInstallResult as InstallResult, SlashCommandInstallStatus,
    SlashCommandScope, COMMAND_NAME,
};
use serde_json::{json, Value};

use crate::analytics::{count_bucket, IntegrationScope, IntegrationTelemetry, TargetSelection};
use crate::output::JsonOutputFormat;
use crate::ui::{
    diagnostic, empty_state, fields, hint, outcome, section, table, Action, Diagnostic,
    DiagnosticLevel, Document, EmptyState, Field, Hint, Outcome, OutcomeState, RenderContext,
    Table, Ui,
};

pub(crate) use ctx_agent_integrations::slash_commands::PathContext;

mod lifecycle;

pub(crate) use lifecycle::{run_remove, run_status};

#[derive(Debug, Args)]
pub(crate) struct SlashCommandInstallArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SlashCommandAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project instead of global agent dirs"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(long, help = "Overwrite locally modified ctx-managed command files")]
    force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SlashCommandStatusArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SlashCommandAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(long, help = "Check the current project instead of global agent dirs")]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct SlashCommandRemoveArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SlashCommandAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Remove from the current project instead of global agent dirs"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(long, help = "Remove locally modified exact command files")]
    force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum SlashCommandAgentArg {
    Codex,
    #[value(name = "grok-build", alias = "grok")]
    GrokBuild,
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    Cursor,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    #[value(name = "mimocode", alias = "mimo-code", alias = "mimo_code")]
    MiMoCode,
    #[value(name = "gemini-cli", alias = "gemini")]
    GeminiCli,
    #[value(name = "qwen-code", alias = "qwen")]
    QwenCode,
    Antigravity,
    #[value(name = "github-copilot", alias = "copilot")]
    GitHubCopilot,
    Pi,
    Goose,
    Continue,
}

impl SlashCommandAgentArg {
    const fn integration(self) -> SlashCommandAgent {
        match self {
            Self::Codex => SlashCommandAgent::Codex,
            Self::GrokBuild => SlashCommandAgent::GrokBuild,
            Self::ClaudeCode => SlashCommandAgent::ClaudeCode,
            Self::Cursor => SlashCommandAgent::Cursor,
            Self::OpenCode => SlashCommandAgent::OpenCode,
            Self::MiMoCode => SlashCommandAgent::MiMoCode,
            Self::GeminiCli => SlashCommandAgent::GeminiCli,
            Self::QwenCode => SlashCommandAgent::QwenCode,
            Self::Antigravity => SlashCommandAgent::Antigravity,
            Self::GitHubCopilot => SlashCommandAgent::GitHubCopilot,
            Self::Pi => SlashCommandAgent::Pi,
            Self::Goose => SlashCommandAgent::Goose,
            Self::Continue => SlashCommandAgent::Continue,
        }
    }
}

pub(crate) fn insert_install_analytics(
    telemetry: &mut IntegrationTelemetry,
    args: &SlashCommandInstallArgs,
) {
    telemetry.scope = Some(if args.project {
        IntegrationScope::Project
    } else {
        IntegrationScope::Global
    });
    telemetry.selection = Some(if args.all_agents {
        TargetSelection::All
    } else if args.agent.is_empty() {
        TargetSelection::Detected
    } else {
        TargetSelection::Explicit
    });
    telemetry.force = Some(args.force);
    telemetry.target_agents = Some(count_bucket(if args.all_agents {
        SlashCommandAgentArg::value_variants().len() as u64
    } else {
        args.agent.len() as u64
    }));
}

pub(crate) fn insert_status_analytics(
    telemetry: &mut IntegrationTelemetry,
    args: &SlashCommandStatusArgs,
) {
    insert_target_analytics(
        telemetry,
        args.agent.len(),
        args.all_agents,
        args.project,
        false,
    );
}

pub(crate) fn insert_remove_analytics(
    telemetry: &mut IntegrationTelemetry,
    args: &SlashCommandRemoveArgs,
) {
    insert_target_analytics(
        telemetry,
        args.agent.len(),
        args.all_agents,
        args.project,
        args.force,
    );
}

fn insert_target_analytics(
    telemetry: &mut IntegrationTelemetry,
    explicit_agents: usize,
    all_agents: bool,
    project: bool,
    force: bool,
) {
    telemetry.scope = Some(if project {
        IntegrationScope::Project
    } else {
        IntegrationScope::Global
    });
    telemetry.selection = Some(if all_agents {
        TargetSelection::All
    } else if explicit_agents == 0 {
        TargetSelection::Detected
    } else {
        TargetSelection::Explicit
    });
    telemetry.force = Some(force);
    telemetry.target_agents = Some(count_bucket(if all_agents {
        SlashCommandAgentArg::value_variants().len() as u64
    } else {
        explicit_agents as u64
    }));
}

pub(crate) fn run_install(
    args: SlashCommandInstallArgs,
    context: &PathContext,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.format.is_json();
    let outcome = application::install(
        SlashCommandInstallApplicationRequest {
            agents: args
                .agent
                .iter()
                .copied()
                .map(SlashCommandAgentArg::integration)
                .collect(),
            all_agents: args.all_agents,
            project: args.project,
            force: args.force,
        },
        context,
        identity,
    )?;
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    let receipt = outcome.receipt;
    if json_output {
        let mut output = serde_json::to_string_pretty(&json!({
            "integration": "slash-commands",
            "command": COMMAND_NAME,
            "scope": if receipt.project { "project" } else { "global" },
            "results": receipt.results.iter().map(install_result_json).collect::<Vec<_>>(),
        }))?;
        output.push('\n');
        ui.write_stdout_bytes(output.as_bytes())?;
    } else {
        let document = render_install_results(ui.stdout_context(), &receipt.results);
        ui.write_stdout(&document)?;
        if let Some(diagnostics) =
            render_install_failures(ui.stderr_context(), identity, &receipt.results)
        {
            ui.write_stderr(&diagnostics)?;
        }
    }
    if receipt.failed > 0 {
        if !json_output {
            return Err(crate::rendered_cli_error());
        }
        return Err(anyhow!(
            "failed to install slash commands for {} target(s)",
            receipt.failed
        ));
    }
    Ok(())
}

fn install_result_json(result: &InstallResult) -> Value {
    json!({
        "agent": result.agent.id(),
        "agent_display_name": result.agent.display_name(),
        "scope": result.scope.map(SlashCommandScope::as_str),
        "path": result.path,
        "success": result.success,
        "previous_status": result.previous_status.as_str(),
        "status": result.status.as_str(),
        "already_installed": result.already_installed,
        "updated": result.updated,
        "migrated": result.migrated,
        "legacy_path": result.legacy_path,
        "error": result.error,
        "note": result.note,
    })
}

fn render_install_results(context: &RenderContext, results: &[InstallResult]) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No separate slash-command targets detected",
                detail: "Select a file-based agent explicitly, or install the bundled Agent Skill.",
                action: Some(Action {
                    command: "ctx integrations install skill",
                }),
            },
        );
    }

    let failed = results.iter().filter(|result| !result.success).count();
    let updated = results.iter().filter(|result| result.updated).count();
    let title = if failed > 0 {
        match failed {
            1 => "1 slash-command target needs attention".to_owned(),
            count => format!("{count} slash-command targets need attention"),
        }
    } else if updated > 0 {
        match updated {
            1 => "1 slash-command target updated".to_owned(),
            count => format!("{count} slash-command targets updated"),
        }
    } else {
        "Slash-command integration is ready".to_owned()
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
            detail: Some("Invoke the installed entry point as /ctx."),
        },
    );
    let mut targets = Table::new(["Agent", "Status", "Location"]);
    for result in results {
        let location = result
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| match result.status {
                SlashCommandInstallStatus::SkillOnly => "Agent Skill".to_owned(),
                SlashCommandInstallStatus::ManualOnly => "manual setup".to_owned(),
                _ => "-".to_owned(),
            });
        targets.push_row([
            result.agent.display_name().to_owned(),
            install_result_verb(result).to_owned(),
            location,
        ]);
    }
    document.push_blank();
    document.append(section("Targets", table(context, &targets)));

    if results
        .iter()
        .any(|result| result.status == SlashCommandInstallStatus::SkillOnly)
    {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Skill-based agents use the bundled Agent Skill.",
            },
            Some(Action {
                command: "ctx integrations install skill",
            }),
        ));
    }
    let manual_notes = results
        .iter()
        .filter(|result| result.status == SlashCommandInstallStatus::ManualOnly)
        .filter_map(|result| {
            result
                .note
                .as_deref()
                .map(|note| (result.agent.display_name(), note))
        })
        .collect::<Vec<_>>();
    if !manual_notes.is_empty() {
        let guidance = manual_notes
            .iter()
            .map(|(agent, note)| Field::new(agent, note))
            .collect::<Vec<_>>();
        document.push_blank();
        document.append(section("Manual setup", fields(context, &guidance)));
    }
    document
}

fn render_install_failures(
    context: &RenderContext,
    identity: ProductIdentity<'_>,
    results: &[InstallResult],
) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| !result.success) {
        let summary = format!("{} was not changed", result.agent.display_name());
        let command = application::force_install_command(identity, result);
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
                action: command.as_deref().map(|command| Action { command }),
            },
        ));
    }
    (!document.is_empty()).then_some(document)
}

fn install_result_verb(result: &InstallResult) -> &'static str {
    if result.already_installed {
        match result.status {
            SlashCommandInstallStatus::SkillOnly => "skill-only",
            SlashCommandInstallStatus::ManualOnly => "manual",
            _ => "current",
        }
    } else if !result.success {
        "skipped"
    } else if result.migrated {
        "migrated"
    } else if result.updated {
        "updated"
    } else {
        "installed"
    }
}

#[cfg(test)]
#[path = "slash_commands/tests.rs"]
mod tests;

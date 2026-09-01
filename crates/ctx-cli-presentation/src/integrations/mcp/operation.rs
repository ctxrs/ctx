use anyhow::{anyhow, Result};
use ctx_agent_application::{integrations::mcp as application, ProductIdentity};
use ctx_agent_integrations::mcp_config::{
    McpInstallRequest, McpInstallResult, McpStatusRequest, McpStatusResult,
};
use serde_json::{json, Value};

use crate::{
    analytics::IntegrationTelemetry,
    ui::{
        diagnostic, empty_state, fields, hint, outcome, section, Action, Diagnostic,
        DiagnosticLevel, Document, EmptyState, Field, Hint, Outcome, OutcomeState, RenderContext,
        Ui,
    },
};

use super::{
    format::{self, ConfigStatus},
    McpInstallArgs, McpPathContext, McpStatusArgs,
};

pub(super) fn run_install(
    args: McpInstallArgs,
    context: &McpPathContext,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let outcome = application::install(
        McpInstallRequest {
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
        let output = format!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": identity.name,
                    "command": command.executable(),
                    "args": command.args(),
                },
                "scope": if receipt.project { "project" } else { "global" },
                "results": receipt.results.iter().map(mcp_install_result_json).collect::<Vec<_>>(),
            })
        );
        ui.write_stdout_bytes(format!("{output}\n").as_bytes())?;
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
        if !args.format.is_json() {
            return Err(crate::rendered_cli_error());
        }
        return Err(anyhow!(
            "failed to install MCP integration for {} target(s)",
            receipt.failed
        ));
    }
    Ok(())
}

pub(super) fn run_status(
    args: McpStatusArgs,
    context: &McpPathContext,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let outcome = application::status(
        McpStatusRequest {
            agents: args.target.agent.clone(),
            all_agents: args.target.all_agents,
            project: args.target.project,
        },
        context,
        identity,
    );
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    let recovery_command = outcome.recovery_command;
    let receipt = outcome.receipt;
    let results = &receipt.results;
    if args.format.is_json() {
        let command = format::server_command();
        let output = format!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": identity.name,
                    "command": command.executable(),
                    "args": command.args(),
                },
                "scope": if receipt.request.project { "project" } else { "global" },
                "results": results.iter().map(mcp_status_result_json).collect::<Vec<_>>(),
            })
        );
        ui.write_stdout_bytes(format!("{output}\n").as_bytes())?;
    } else {
        let document =
            render_status_results(ui.stdout_context(), results, recovery_command.as_deref());
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn mcp_install_result_json(result: &McpInstallResult) -> Value {
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
        "already_installed": result.already_installed,
        "modified": result.modified,
        "error": result.error,
    })
}

fn mcp_status_result_json(result: &McpStatusResult) -> Value {
    json!({
        "agent": result.target.agent.id(),
        "agent_display_name": result.target.agent.display_name(),
        "scope": result.target.scope.as_str(),
        "path": result.target.path,
        "detected": result.target.detected,
        "supported": result.target.unsupported_reason.is_none(),
        "status": result.status.as_str(),
        "error": result.error,
    })
}

fn render_install_results(context: &RenderContext, results: &[McpInstallResult]) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No MCP-capable coding agents detected",
                detail: "Select a coding agent explicitly or install every supported target.",
                action: Some(Action {
                    command: "ctx integrations install mcp --all-agents",
                }),
            },
        );
    }
    let all_current = results.iter().all(|result| result.already_installed);
    let all_success = results.iter().all(|result| result.success);
    let any_modified = results.iter().any(|result| result.modified);
    let title = if all_current {
        "ctx MCP integration is already installed"
    } else if all_success && any_modified {
        "ctx MCP integration installed"
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
    let command = format::server_command().render_for_host();
    document.push_blank();
    document.append(fields(context, &[Field::new("Server", &command)]));

    let rows = results
        .iter()
        .map(|result| {
            let status = if result.already_installed {
                "current"
            } else if result.modified {
                "modified"
            } else if result.success {
                "ok"
            } else {
                "skipped"
            };
            (status, mcp_install_target_detail(result))
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

fn render_install_failures(
    context: &RenderContext,
    identity: ProductIdentity<'_>,
    results: &[McpInstallResult],
) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| !result.success) {
        let summary = format!(
            "{} MCP configuration was not changed",
            result.target.agent.display_name()
        );
        let command = application::force_install_command(identity, &result.target);
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

fn render_status_results(
    context: &RenderContext,
    results: &[McpStatusResult],
    recovery_command: Option<&str>,
) -> Document {
    if results.is_empty() {
        return empty_state(
            context,
            EmptyState {
                title: "No MCP-capable coding agents detected",
                detail: "Select a coding agent explicitly or inspect every supported target.",
                action: Some(Action {
                    command: "ctx integrations status mcp --all-agents",
                }),
            },
        );
    }
    let all_current = results
        .iter()
        .all(|result| result.status == ConfigStatus::Current);
    let mut document = outcome(
        context,
        Outcome {
            state: if all_current {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: if all_current {
                "ctx MCP integration is current"
            } else {
                "ctx MCP integration needs attention"
            },
            detail: None,
        },
    );
    let command = format::server_command().render_for_host();
    document.push_blank();
    document.append(fields(context, &[Field::new("Server", &command)]));

    let rows = results
        .iter()
        .map(|result| (result.status.as_str(), mcp_status_target_detail(result)))
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    if let Some(command) = recovery_command {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Install or refresh MCP configuration for the affected targets.",
            },
            Some(Action { command }),
        ));
    }
    document
}

fn mcp_install_target_detail(result: &McpInstallResult) -> String {
    let mut detail = result.target.agent.display_name().to_owned();
    if let Some(path) = &result.target.path {
        detail.push_str(" -> ");
        detail.push_str(&path.display().to_string());
    }
    detail
}

fn mcp_status_target_detail(result: &McpStatusResult) -> String {
    let mut detail = format!(
        "{} ({})",
        result.target.agent.display_name(),
        result.target.scope.as_str()
    );
    if let Some(path) = &result.target.path {
        detail.push_str(" -> ");
        detail.push_str(&path.display().to_string());
    }
    if let Some(error) = &result.error {
        detail.push_str(" - ");
        detail.push_str(error);
    }
    detail
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext, Token};
    use ctx_agent_integrations::mcp_config::{install_target, status_target, McpAgentArg};

    const PRODUCT: ProductIdentity<'static> = ProductIdentity {
        name: "ctx",
        version: "1.0.0-test",
    };

    fn render_context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    fn semantic_command(document: &Document) -> String {
        document
            .lines()
            .iter()
            .flat_map(|line| line.spans())
            .filter(|span| span.token() == Token::Command)
            .map(|span| span.content())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn human_install_and_status_results_use_the_typed_ui() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let path_context = McpPathContext::for_tests(home, temp.path().join("repo"));
        let target = McpAgentArg::QwenCode.target(false, &path_context);
        let missing = status_target(&target);
        let installed = install_target(&target, false);
        let current = status_target(&target);

        for (document, expected) in [
            (
                render_install_results(&render_context(80, ColorMode::Never), &[installed]),
                "ctx MCP integration installed",
            ),
            (
                render_status_results(
                    &render_context(80, ColorMode::Never),
                    &[missing],
                    Some("ctx integrations install mcp --agent qwen-code"),
                ),
                "ctx MCP integration needs attention",
            ),
            (
                render_status_results(&render_context(80, ColorMode::Never), &[current], None),
                "ctx MCP integration is current",
            ),
        ] {
            let plain = document.render_plain();
            assert!(plain.contains(expected), "{plain}");
            assert!(plain.contains("Server"), "{plain}");
            assert!(plain.contains("Targets"), "{plain}");
        }

        let color = render_context(80, ColorMode::Always);
        let document = render_status_results(&color, &[status_target(&target)], None);
        let styled = document.render(&color);
        assert!(styled.as_bytes().contains(&0x1b), "{styled:?}");
        assert_eq!(strip_ansi(&styled), document.render_plain());
    }

    #[test]
    fn missing_mcp_status_offers_the_exact_selected_install_action() {
        let path_context = McpPathContext::for_tests("/home/test".into(), "/repo/test".into());
        let result = McpStatusResult {
            target: McpAgentArg::Codex.target(true, &path_context),
            status: ConfigStatus::Missing,
            error: None,
        };

        let command = "ctx integrations install mcp --agent codex --project".to_owned();
        assert_eq!(
            command,
            "ctx integrations install mcp --agent codex --project"
        );
        for width in [32, 48, 80, 120] {
            let context = render_context(width, ColorMode::Never);
            let document =
                render_status_results(&context, std::slice::from_ref(&result), Some(&command));
            assert_eq!(semantic_command(&document), command);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                normalized.contains("Install or refresh MCP configuration"),
                "{rendered}"
            );
        }
    }

    #[test]
    fn mcp_conflict_names_the_selected_agent_in_the_force_action() {
        let path_context = McpPathContext::for_tests("/home/test".into(), "/repo/test".into());

        for (agent, project, expected_agent) in [
            (McpAgentArg::Cursor, false, "cursor"),
            (McpAgentArg::Codex, true, "codex"),
        ] {
            let result = McpInstallResult {
                target: agent.target(project, &path_context),
                success: false,
                previous_status: ConfigStatus::Conflict,
                status: ConfigStatus::Conflict,
                already_installed: false,
                modified: false,
                error: Some(
                    "existing ctx MCP server has different command or args; rerun with --force to overwrite"
                        .to_owned(),
                ),
            };
            let expected_project = if project { " --project" } else { "" };
            let expected = format!(
                "ctx integrations install mcp --agent {expected_agent}{expected_project} --force"
            );

            for width in [32, 48, 80, 120] {
                let plain_context = RenderContext::for_test(
                    TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Never),
                );
                let styled_context = RenderContext::for_test(
                    TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Always),
                );
                let plain_document =
                    render_install_failures(&plain_context, PRODUCT, std::slice::from_ref(&result))
                        .unwrap();
                let styled_document = render_install_failures(
                    &styled_context,
                    PRODUCT,
                    std::slice::from_ref(&result),
                )
                .unwrap();

                assert_eq!(semantic_command(&plain_document), expected);
                let normalized = plain_document
                    .render_plain()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(normalized.contains(&format!(
                    "{} MCP configuration was not changed",
                    agent.display_name()
                )));
                assert_eq!(
                    strip_ansi(&styled_document.render(&styled_context)),
                    plain_document.render_plain()
                );
            }
        }
    }
}

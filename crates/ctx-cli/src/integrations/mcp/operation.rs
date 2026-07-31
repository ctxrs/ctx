use std::{fs, io, path::Path};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::{
    analytics::{count_bucket, IntegrationResult, IntegrationTelemetry},
    ui::{
        diagnostic, empty_state, fields, hint, outcome, section, Action, Diagnostic,
        DiagnosticLevel, Document, EmptyState, Field, Hint, Outcome, OutcomeState, RenderContext,
        Ui,
    },
};

use super::{
    format::{self, ConfigKind, ConfigStatus},
    registry::{project_detection_path, McpTarget},
    McpAgentArg, McpInstallArgs, McpPathContext, McpStatusArgs, SERVER_NAME,
};

#[derive(Debug)]
struct McpInstallResult {
    target: McpTarget,
    success: bool,
    previous_status: ConfigStatus,
    status: ConfigStatus,
    already_installed: bool,
    modified: bool,
    error: Option<String>,
}

impl McpInstallResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "path": self.target.path,
            "detected": self.target.detected,
            "supported": self.target.unsupported_reason.is_none(),
            "success": self.success,
            "previous_status": self.previous_status.as_str(),
            "status": self.status.as_str(),
            "already_installed": self.already_installed,
            "modified": self.modified,
            "error": self.error,
        })
    }
}

#[derive(Debug)]
struct McpStatusResult {
    target: McpTarget,
    status: ConfigStatus,
    error: Option<String>,
}

impl McpStatusResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "path": self.target.path,
            "detected": self.target.detected,
            "supported": self.target.unsupported_reason.is_none(),
            "status": self.status.as_str(),
            "error": self.error,
        })
    }
}

pub(super) fn run_install(
    args: McpInstallArgs,
    context: &McpPathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let agents = selected_install_agents(&args, context);
    insert_selection_analytics(telemetry, &agents);
    let targets = agents
        .iter()
        .copied()
        .map(|agent| agent.target(args.project, context))
        .collect::<Vec<_>>();
    let results = targets
        .iter()
        .map(|target| install_target(target, args.force))
        .collect::<Vec<_>>();
    let failed = results.iter().filter(|result| !result.success).count();
    telemetry.result = Some(if failed == 0 {
        IntegrationResult::Ok
    } else {
        IntegrationResult::PartialError
    });
    telemetry.modified_targets = Some(count_bucket(
        results.iter().filter(|result| result.modified).count() as u64,
    ));
    if args.format.is_json() {
        let command = format::server_command();
        println!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": SERVER_NAME,
                    "command": command.executable(),
                    "args": command.args(),
                },
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(McpInstallResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let document = render_install_results(ui.stdout_context(), &results);
        ui.write_stdout(&document)?;
        if let Some(diagnostics) = render_install_failures(ui.stderr_context(), &results) {
            ui.write_stderr(&diagnostics)?;
        }
    }
    if failed > 0 {
        if !args.format.is_json() {
            return Err(crate::dispatch::rendered_cli_error());
        }
        return Err(anyhow!(
            "failed to install MCP integration for {failed} target(s)"
        ));
    }
    Ok(())
}

pub(super) fn run_status(
    args: McpStatusArgs,
    context: &McpPathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let agents = selected_status_agents(&args, context);
    insert_selection_analytics(telemetry, &agents);
    let targets = agents
        .iter()
        .copied()
        .map(|agent| agent.target(args.project, context))
        .collect::<Vec<_>>();
    let results = targets.iter().map(status_target).collect::<Vec<_>>();
    let status_count = |status| {
        results
            .iter()
            .filter(|result| result.status == status)
            .count()
    };
    telemetry.current_targets = Some(count_bucket(status_count(ConfigStatus::Current) as u64));
    telemetry.missing_targets = Some(count_bucket(status_count(ConfigStatus::Missing) as u64));
    telemetry.conflicting_targets = Some(count_bucket(status_count(ConfigStatus::Conflict) as u64));
    telemetry.invalid_targets = Some(count_bucket(status_count(ConfigStatus::Invalid) as u64));
    telemetry.unsupported_targets =
        Some(count_bucket(status_count(ConfigStatus::Unsupported) as u64));
    let current = status_count(ConfigStatus::Current);
    telemetry.result = Some(if current == results.len() {
        IntegrationResult::AllCurrent
    } else if current == 0 {
        IntegrationResult::NoneCurrent
    } else {
        IntegrationResult::PartiallyCurrent
    });
    if args.format.is_json() {
        let command = format::server_command();
        println!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": SERVER_NAME,
                    "command": command.executable(),
                    "args": command.args(),
                },
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(McpStatusResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let recovery_command = status_install_command(&args, &results);
        let document =
            render_status_results(ui.stdout_context(), &results, recovery_command.as_deref());
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn insert_selection_analytics(telemetry: &mut IntegrationTelemetry, agents: &[McpAgentArg]) {
    telemetry.resolved_agents = Some(count_bucket(agents.len() as u64));
}

fn selected_install_agents(args: &McpInstallArgs, context: &McpPathContext) -> Vec<McpAgentArg> {
    if args.all_agents {
        return if args.project {
            McpAgentArg::PROJECT_CAPABLE.to_vec()
        } else {
            McpAgentArg::ALL.to_vec()
        };
    }
    if !args.agent.is_empty() {
        return dedupe_agents(args.agent.iter().copied());
    }
    if args.project {
        return detected_project_agents(context);
    }
    detected_agents(context)
}

fn selected_status_agents(args: &McpStatusArgs, context: &McpPathContext) -> Vec<McpAgentArg> {
    if args.all_agents {
        return if args.project {
            McpAgentArg::PROJECT_CAPABLE.to_vec()
        } else {
            McpAgentArg::ALL.to_vec()
        };
    }
    if !args.agent.is_empty() {
        return dedupe_agents(args.agent.iter().copied());
    }
    if args.project {
        return detected_project_agents(context);
    }
    detected_agents(context)
}

fn dedupe_agents(agents: impl IntoIterator<Item = McpAgentArg>) -> Vec<McpAgentArg> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

fn detected_agents(context: &McpPathContext) -> Vec<McpAgentArg> {
    McpAgentArg::ALL
        .iter()
        .copied()
        .filter(|agent| agent.detected(context))
        .collect()
}

fn detected_project_agents(context: &McpPathContext) -> Vec<McpAgentArg> {
    McpAgentArg::PROJECT_CAPABLE
        .iter()
        .copied()
        .filter(|agent| project_detection_path(*agent, context).exists())
        .collect()
}

fn install_target(target: &McpTarget, force: bool) -> McpInstallResult {
    let previous = status_target(target);
    if previous.status == ConfigStatus::Current {
        return McpInstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: ConfigStatus::Current,
            already_installed: true,
            modified: false,
            error: None,
        };
    }
    if matches!(
        previous.status,
        ConfigStatus::Unsupported | ConfigStatus::Invalid
    ) {
        return McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            modified: false,
            error: previous.error,
        };
    }
    if previous.status == ConfigStatus::Conflict && !force {
        return McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            modified: false,
            error: Some(
                "existing ctx MCP server has different command or args; rerun with --force to overwrite"
                    .to_owned(),
            ),
        };
    }
    let result = write_target(target, force);
    match result {
        Ok(()) => McpInstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: ConfigStatus::Current,
            already_installed: false,
            modified: true,
            error: None,
        },
        Err(err) => McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: ConfigStatus::Invalid,
            already_installed: false,
            modified: false,
            error: Some(err.to_string()),
        },
    }
}

fn status_target(target: &McpTarget) -> McpStatusResult {
    let Some(path) = target.path.as_ref() else {
        return McpStatusResult {
            target: target.clone(),
            status: ConfigStatus::Unsupported,
            error: target.unsupported_reason.clone(),
        };
    };
    let Some(kind) = target.kind else {
        return McpStatusResult {
            target: target.clone(),
            status: ConfigStatus::Unsupported,
            error: target.unsupported_reason.clone(),
        };
    };
    match read_target_status(path, kind) {
        Ok(status) => McpStatusResult {
            target: target.clone(),
            status,
            error: None,
        },
        Err(err) => McpStatusResult {
            target: target.clone(),
            status: ConfigStatus::Invalid,
            error: Some(err.to_string()),
        },
    }
}

fn read_target_status(path: &Path, kind: ConfigKind) -> Result<ConfigStatus> {
    let body = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(ConfigStatus::Missing),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    if body.trim().is_empty() {
        return Ok(ConfigStatus::Missing);
    }
    format::status(&body, kind, path)
}

fn write_target(target: &McpTarget, force: bool) -> Result<()> {
    let path = target
        .path
        .as_ref()
        .ok_or_else(|| anyhow!("unsupported MCP target"))?;
    let kind = target
        .kind
        .ok_or_else(|| anyhow!("unsupported MCP target"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let existing = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let body = format::upsert(&existing, kind, force, path)?;
    fs::write(path, body).with_context(|| format!("write {}", path.display()))
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
    results: &[McpInstallResult],
) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| !result.success) {
        let summary = format!(
            "{} MCP configuration was not changed",
            result.target.agent.display_name()
        );
        let command = force_install_command(&result.target);
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

fn force_install_command(target: &McpTarget) -> String {
    let project = if target.scope.as_str() == "project" {
        " --project"
    } else {
        ""
    };
    format!(
        "ctx integrations install mcp --agent {}{project} --force",
        target.agent.id()
    )
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

fn status_install_command(args: &McpStatusArgs, results: &[McpStatusResult]) -> Option<String> {
    let repairable = results
        .iter()
        .filter(|result| {
            matches!(
                result.status,
                ConfigStatus::Missing | ConfigStatus::Conflict
            )
        })
        .collect::<Vec<_>>();
    if repairable.is_empty() {
        return None;
    }

    let mut tokens = ["ctx", "integrations", "install", "mcp"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let has_unrepairable = results.iter().any(|result| {
        matches!(
            result.status,
            ConfigStatus::Invalid | ConfigStatus::Unsupported
        )
    });
    if args.all_agents && !has_unrepairable {
        tokens.push("--all-agents".to_owned());
    } else if !args.agent.is_empty() && !has_unrepairable {
        for agent in dedupe_agents(args.agent.iter().copied()) {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    } else {
        for agent in dedupe_agents(repairable.iter().map(|result| result.target.agent)) {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    }
    if args.project {
        tokens.push("--project".to_owned());
    }
    if results
        .iter()
        .any(|result| result.status == ConfigStatus::Conflict)
    {
        tokens.push("--force".to_owned());
    }
    Some(tokens.join(" "))
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
    use std::{fs, io::Write as _};

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext, Token};

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
    fn status_reports_unsupported_project_target() {
        let temp = tempfile::tempdir().unwrap();
        let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let target = McpAgentArg::GitHubCopilot.target(true, &context);
        let status = status_target(&target);
        assert_eq!(status.status, ConfigStatus::Unsupported);
        assert_eq!(
            status.error.as_deref(),
            Some("project-scoped MCP config is not documented for this agent")
        );
    }

    #[test]
    fn current_target_is_not_rewritten() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let context = McpPathContext::for_tests(home, temp.path().join("repo"));
        let target = McpAgentArg::QwenCode.target(false, &context);
        let path = target.path.as_ref().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = b"{\n  \"unrelated\": true,\n  \"mcpServers\": {\n    \"ctx\": {\"command\": \"ctx\", \"args\": [\"mcp\", \"serve\"]}\n  }\n}\n";
        fs::write(path, original).unwrap();

        let result = install_target(&target, false);

        assert!(result.success);
        assert!(result.already_installed);
        assert!(!result.modified);
        assert_eq!(fs::read(path).unwrap(), original);
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
        let args = McpStatusArgs {
            agent: vec![McpAgentArg::Codex],
            all_agents: false,
            project: true,
            format: crate::output::JsonOutputFormat::Text,
        };
        let result = McpStatusResult {
            target: McpAgentArg::Codex.target(true, &path_context),
            status: ConfigStatus::Missing,
            error: None,
        };

        let command = status_install_command(&args, std::slice::from_ref(&result)).unwrap();
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
                    render_install_failures(&plain_context, std::slice::from_ref(&result)).unwrap();
                let styled_document =
                    render_install_failures(&styled_context, std::slice::from_ref(&result))
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

    #[cfg(unix)]
    #[test]
    fn update_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let context = McpPathContext::for_tests(home, temp.path().join("repo"));
        let target = McpAgentArg::QwenCode.target(false, &context);
        let path = target.path.as_ref().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "{\"unrelated\":true}").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();

        let result = install_target(&target, false);

        assert!(result.success);
        assert!(result.modified);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

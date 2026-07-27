use std::{fs, io, path::Path};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::analytics::{count_bucket, IntegrationResult, IntegrationTelemetry};

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
    if args.json {
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
        print_install_results(&results);
    }
    if failed > 0 {
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
    if args.json {
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
        print_status_results(&results);
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

fn print_install_results(results: &[McpInstallResult]) {
    if results.is_empty() {
        println!("No detected MCP-capable coding agents found.");
        println!("Use --agent <name> or --all-agents to install a specific MCP config.");
        return;
    }
    let all_current = results.iter().all(|result| result.already_installed);
    let all_success = results.iter().all(|result| result.success);
    let any_modified = results.iter().any(|result| result.modified);
    let heading = if all_current {
        "ctx MCP integration already installed"
    } else if all_success && any_modified {
        "ctx MCP integration installed"
    } else {
        "ctx MCP integration"
    };
    println!("{heading}: {}", format::server_command().render_for_host());
    for result in results {
        let verb = if result.already_installed {
            "current"
        } else if result.modified {
            "modified"
        } else if result.success {
            "ok"
        } else {
            "skipped"
        };
        let detail = result
            .error
            .as_deref()
            .map(|error| format!(" - {error}"))
            .unwrap_or_default();
        let path = result
            .target
            .path
            .as_ref()
            .map(|path| format!(" -> {}", path.display()))
            .unwrap_or_default();
        println!(
            "  {verb}: {}{}{}",
            result.target.agent.display_name(),
            path,
            detail
        );
    }
}

fn print_status_results(results: &[McpStatusResult]) {
    if results.is_empty() {
        println!("No detected MCP-capable coding agents found.");
        println!("Use --agent <name> or --all-agents to inspect a specific MCP config.");
        return;
    }
    println!(
        "ctx MCP integration status: {}",
        format::server_command().render_for_host()
    );
    for result in results {
        let detail = result
            .error
            .as_deref()
            .map(|error| format!(" - {error}"))
            .unwrap_or_default();
        let path = result
            .target
            .path
            .as_ref()
            .map(|path| format!(" -> {}", path.display()))
            .unwrap_or_default();
        println!(
            "  {}: {} ({}){}{}",
            result.status.as_str(),
            result.target.agent.display_name(),
            result.target.scope.as_str(),
            path,
            detail
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

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

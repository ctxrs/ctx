//! MCP client configuration workflows independent of CLI presentation.

use ctx_agent_integrations::mcp_config::{
    dedupe_agents, execute_install, execute_remove, execute_status, ConfigStatus,
    McpInstallReceipt, McpInstallRequest, McpPathContext, McpRemoveReceipt, McpRemoveRequest,
    McpStatusReceipt, McpStatusRequest, McpTarget,
};

use crate::{IntegrationResultFact, IntegrationTelemetryFacts, ProductIdentity};

#[derive(Debug)]
pub struct McpInstallOutcome {
    pub receipt: McpInstallReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

#[derive(Debug)]
pub struct McpStatusOutcome {
    pub receipt: McpStatusReceipt,
    pub recovery_command: Option<String>,
    pub telemetry: IntegrationTelemetryFacts,
}

#[derive(Debug)]
pub struct McpRemoveOutcome {
    pub receipt: McpRemoveReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

pub fn install(request: McpInstallRequest, context: &McpPathContext) -> McpInstallOutcome {
    let receipt = execute_install(request, context);
    let telemetry = IntegrationTelemetryFacts {
        resolved_agents: Some(receipt.selected_agents),
        result: Some(if receipt.failed == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        modified_targets: Some(receipt.modified),
        ..IntegrationTelemetryFacts::default()
    };
    McpInstallOutcome { receipt, telemetry }
}

pub fn remove(request: McpRemoveRequest, context: &McpPathContext) -> McpRemoveOutcome {
    let receipt = execute_remove(request, context);
    let telemetry = IntegrationTelemetryFacts {
        resolved_agents: Some(receipt.selected_agents),
        result: Some(if receipt.failed == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        modified_targets: Some(receipt.modified),
        ..IntegrationTelemetryFacts::default()
    };
    McpRemoveOutcome { receipt, telemetry }
}

pub fn status(
    request: McpStatusRequest,
    context: &McpPathContext,
    identity: ProductIdentity<'_>,
) -> McpStatusOutcome {
    let receipt = execute_status(request, context);
    let status_count = |status| {
        receipt
            .results
            .iter()
            .filter(|result| result.status == status)
            .count()
    };
    let current = status_count(ConfigStatus::Current);
    let telemetry = IntegrationTelemetryFacts {
        resolved_agents: Some(receipt.selected_agents),
        result: Some(if current == receipt.results.len() {
            IntegrationResultFact::AllCurrent
        } else if current == 0 {
            IntegrationResultFact::NoneCurrent
        } else {
            IntegrationResultFact::PartiallyCurrent
        }),
        current_targets: Some(current),
        missing_targets: Some(status_count(ConfigStatus::Missing)),
        conflicting_targets: Some(status_count(ConfigStatus::Conflict)),
        invalid_targets: Some(status_count(ConfigStatus::Invalid)),
        unsupported_targets: Some(status_count(ConfigStatus::Unsupported)),
        ..IntegrationTelemetryFacts::default()
    };
    let recovery_command = status_install_command(identity, &receipt.request, &receipt.results);
    McpStatusOutcome {
        receipt,
        recovery_command,
        telemetry,
    }
}

pub fn force_install_command(identity: ProductIdentity<'_>, target: &McpTarget) -> String {
    let project = if target.scope.as_str() == "project" {
        " --project"
    } else {
        ""
    };
    format!(
        "{} integrations install mcp --agent {}{project} --force",
        identity.name,
        target.agent.id()
    )
}

pub fn force_remove_command(identity: ProductIdentity<'_>, target: &McpTarget) -> String {
    let project = if target.scope.as_str() == "project" {
        " --project"
    } else {
        ""
    };
    format!(
        "{} integrations remove mcp --agent {}{project} --force",
        identity.name,
        target.agent.id()
    )
}

fn status_install_command(
    identity: ProductIdentity<'_>,
    request: &McpStatusRequest,
    results: &[ctx_agent_integrations::mcp_config::McpStatusResult],
) -> Option<String> {
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

    let mut tokens = [identity.name, "integrations", "install", "mcp"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let has_unrepairable = results.iter().any(|result| {
        matches!(
            result.status,
            ConfigStatus::Invalid | ConfigStatus::Unsupported
        )
    });
    if request.all_agents && !has_unrepairable {
        tokens.push("--all-agents".to_owned());
    } else if !request.agents.is_empty() && !has_unrepairable {
        for agent in dedupe_agents(request.agents.iter().copied()) {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    } else {
        for agent in dedupe_agents(repairable.iter().map(|result| result.target.agent)) {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    }
    if request.project {
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

#[cfg(test)]
mod tests;

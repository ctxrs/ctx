//! Slash-command installation orchestration independent of CLI rendering.

use anyhow::Result;
use ctx_agent_integrations::slash_commands::{
    execute_install, execute_remove, execute_status, PathContext, SlashCommandAgent,
    SlashCommandInstallReceipt, SlashCommandInstallRequest, SlashCommandInstallResult,
    SlashCommandInstallStatus, SlashCommandRemoveReceipt, SlashCommandRemoveRequest,
    SlashCommandRemoveResult, SlashCommandStatusReceipt, SlashCommandStatusRequest,
    SlashCommandStatusResult,
};

use crate::{IntegrationResultFact, IntegrationTelemetryFacts, ProductIdentity};

#[derive(Debug)]
pub struct SlashCommandInstallOutcome {
    pub receipt: SlashCommandInstallReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

#[derive(Debug)]
pub struct SlashCommandStatusOutcome {
    pub receipt: SlashCommandStatusReceipt,
    pub recovery_command: Option<String>,
    pub telemetry: IntegrationTelemetryFacts,
}

#[derive(Debug)]
pub struct SlashCommandRemoveOutcome {
    pub receipt: SlashCommandRemoveReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

#[derive(Debug, Clone)]
pub struct SlashCommandInstallApplicationRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct SlashCommandStatusApplicationRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
}

#[derive(Debug, Clone)]
pub struct SlashCommandRemoveApplicationRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
    pub force: bool,
}

pub fn install(
    request: SlashCommandInstallApplicationRequest,
    context: &PathContext,
    identity: ProductIdentity<'_>,
) -> Result<SlashCommandInstallOutcome> {
    let receipt = execute_install(
        SlashCommandInstallRequest {
            agents: request.agents,
            all_agents: request.all_agents,
            project: request.project,
            force: request.force,
            product_version: identity.version.to_owned(),
        },
        context,
    )?;
    let telemetry = IntegrationTelemetryFacts {
        resolved_agents: Some(receipt.results.len()),
        result: Some(if receipt.failed == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        already_installed: Some(receipt.already_installed),
        updated: Some(receipt.updated),
        modified_targets: Some(receipt.modified_targets),
        ..IntegrationTelemetryFacts::default()
    };
    Ok(SlashCommandInstallOutcome { receipt, telemetry })
}

pub fn status(
    request: SlashCommandStatusApplicationRequest,
    context: &PathContext,
    identity: ProductIdentity<'_>,
) -> SlashCommandStatusOutcome {
    let receipt = execute_status(
        SlashCommandStatusRequest {
            agents: request.agents,
            all_agents: request.all_agents,
            project: request.project,
        },
        context,
    );
    let count = |status| {
        receipt
            .results
            .iter()
            .filter(|result| result.status == status)
            .count()
    };
    let current = count(SlashCommandInstallStatus::Current);
    let ready = receipt
        .results
        .iter()
        .filter(|result| {
            result.success
                && matches!(
                    result.status,
                    SlashCommandInstallStatus::Current
                        | SlashCommandInstallStatus::SkillOnly
                        | SlashCommandInstallStatus::ManualOnly
                )
        })
        .count();
    let telemetry = IntegrationTelemetryFacts {
        resolved_agents: Some(receipt.selected_agents),
        result: Some(if ready == 0 {
            IntegrationResultFact::NoneCurrent
        } else if ready == receipt.results.len() {
            IntegrationResultFact::AllCurrent
        } else {
            IntegrationResultFact::PartiallyCurrent
        }),
        current_targets: Some(current),
        missing_targets: Some(count(SlashCommandInstallStatus::Missing)),
        conflicting_targets: Some(count(SlashCommandInstallStatus::Stale)),
        modified_targets: Some(count(SlashCommandInstallStatus::Modified)),
        ..IntegrationTelemetryFacts::default()
    };
    let recovery_command = status_install_command(identity, &receipt.request, &receipt.results);
    SlashCommandStatusOutcome {
        receipt,
        recovery_command,
        telemetry,
    }
}

pub fn remove(
    request: SlashCommandRemoveApplicationRequest,
    context: &PathContext,
) -> SlashCommandRemoveOutcome {
    let receipt = execute_remove(
        SlashCommandRemoveRequest {
            agents: request.agents,
            all_agents: request.all_agents,
            project: request.project,
            force: request.force,
        },
        context,
    );
    let telemetry = IntegrationTelemetryFacts {
        resolved_agents: Some(receipt.selected_agents),
        result: Some(if receipt.failed == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        modified_targets: Some(receipt.modified_targets),
        ..IntegrationTelemetryFacts::default()
    };
    SlashCommandRemoveOutcome { receipt, telemetry }
}

pub fn force_install_command(
    identity: ProductIdentity<'_>,
    result: &SlashCommandInstallResult,
) -> Option<String> {
    (result.status == SlashCommandInstallStatus::Modified).then(|| {
        let project = if result
            .scope
            .is_some_and(|scope| scope.as_str() == "project")
        {
            " --project"
        } else {
            ""
        };
        format!(
            "{} integrations install slash-command --agent {}{project} --force",
            identity.name,
            result.agent.id()
        )
    })
}

pub fn force_remove_command(
    identity: ProductIdentity<'_>,
    result: &SlashCommandRemoveResult,
) -> Option<String> {
    result.force_required.then(|| {
        let project = if result
            .scope
            .is_some_and(|scope| scope.as_str() == "project")
        {
            " --project"
        } else {
            ""
        };
        format!(
            "{} integrations remove slash-command --agent {}{project} --force",
            identity.name,
            result.agent.id()
        )
    })
}

fn status_install_command(
    identity: ProductIdentity<'_>,
    request: &SlashCommandStatusRequest,
    results: &[SlashCommandStatusResult],
) -> Option<String> {
    let repairable = results
        .iter()
        .filter(|result| {
            result.success
                && result.scope.is_some()
                && match result.status {
                    SlashCommandInstallStatus::Missing | SlashCommandInstallStatus::Stale => true,
                    SlashCommandInstallStatus::Modified => result.force_required,
                    SlashCommandInstallStatus::Current
                    | SlashCommandInstallStatus::SkillOnly
                    | SlashCommandInstallStatus::ManualOnly => false,
                }
        })
        .collect::<Vec<_>>();
    if repairable.is_empty() {
        return None;
    }

    let mut tokens = [identity.name, "integrations", "install", "slash-command"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let force_required = repairable.iter().any(|result| result.force_required);
    if request.all_agents && results.iter().all(|result| result.success) {
        tokens.push("--all-agents".to_owned());
    } else {
        for result in repairable {
            tokens.extend(["--agent".to_owned(), result.agent.id().to_owned()]);
        }
    }
    if request.project {
        tokens.push("--project".to_owned());
    }
    if force_required {
        tokens.push("--force".to_owned());
    }
    Some(tokens.join(" "))
}

#[cfg(test)]
mod tests;

use anyhow::Result;
use ctx_agent_integrations::skill::{
    execute_install, execute_status, PathContext, SkillAgentSelection, SkillInstallReceipt,
    SkillInstallRequest, SkillInstallStatus, SkillSelectionSource, SkillStatusReceipt,
    SkillStatusRequest, SkillTarget, StatusResult,
};

use crate::{
    IntegrationResultFact, IntegrationTelemetryFacts, ProductIdentity, TargetSelectionFact,
};

#[derive(Debug)]
pub struct SkillInstallOutcome {
    pub receipt: SkillInstallReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

#[derive(Debug)]
pub struct SkillStatusOutcome {
    pub receipt: SkillStatusReceipt,
    pub recovery_command: String,
    pub telemetry: IntegrationTelemetryFacts,
}

pub fn install(
    selection: SkillAgentSelection,
    project: bool,
    force: bool,
    context: &PathContext,
    identity: ProductIdentity<'_>,
) -> Result<SkillInstallOutcome> {
    let selection_fact = selection_fact(selection.source);
    let resolved_agents = selection.agents.len();
    let receipt = execute_install(
        SkillInstallRequest {
            selection,
            project,
            force,
            product_version: identity.version.to_owned(),
        },
        context,
    )?;
    let telemetry = IntegrationTelemetryFacts {
        selection: Some(selection_fact),
        resolved_agents: Some(resolved_agents),
        result: Some(if receipt.fatal_failures == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        already_installed: Some(receipt.already_installed),
        updated: Some(receipt.updated),
        modified_targets: Some(receipt.modified_targets),
        ..IntegrationTelemetryFacts::default()
    };
    Ok(SkillInstallOutcome { receipt, telemetry })
}

pub fn status(
    selection: SkillAgentSelection,
    project: bool,
    context: &PathContext,
    identity: ProductIdentity<'_>,
) -> Result<SkillStatusOutcome> {
    let selection_fact = selection_fact(selection.source);
    let receipt = execute_status(SkillStatusRequest { selection, project }, context)?;
    let telemetry = IntegrationTelemetryFacts {
        selection: Some(selection_fact),
        resolved_agents: Some(receipt.selection.agents.len()),
        result: Some(if receipt.current_count == receipt.results.len() {
            IntegrationResultFact::AllCurrent
        } else if receipt.current_count == 0 {
            IntegrationResultFact::NoneCurrent
        } else {
            IntegrationResultFact::PartiallyCurrent
        }),
        current_targets: Some(receipt.current_count),
        missing_targets: Some(status_count(&receipt.results, SkillInstallStatus::Missing)),
        conflicting_targets: Some(status_count(&receipt.results, SkillInstallStatus::Modified)),
        ..IntegrationTelemetryFacts::default()
    };
    let recovery_command =
        status_install_command(identity, &receipt.selection, project, &receipt.results);
    Ok(SkillStatusOutcome {
        receipt,
        recovery_command,
        telemetry,
    })
}

pub fn force_install_command(identity: ProductIdentity<'_>, target: &SkillTarget) -> String {
    let project = if target.scope.as_str() == "project" {
        " --project"
    } else {
        ""
    };
    format!(
        "{} integrations install skill --agent {}{project} --force",
        identity.name,
        target.agent.id()
    )
}

pub(super) fn selection_fact(source: SkillSelectionSource) -> TargetSelectionFact {
    match source {
        SkillSelectionSource::Explicit => TargetSelectionFact::Explicit,
        SkillSelectionSource::All => TargetSelectionFact::All,
        SkillSelectionSource::Picker => TargetSelectionFact::Picker,
        SkillSelectionSource::Detected => TargetSelectionFact::Detected,
        SkillSelectionSource::Fallback => TargetSelectionFact::Fallback,
    }
}

fn status_count(results: &[StatusResult], status: SkillInstallStatus) -> usize {
    results
        .iter()
        .filter(|result| result.status == status)
        .count()
}

fn status_install_command(
    identity: ProductIdentity<'_>,
    selection: &SkillAgentSelection,
    project: bool,
    results: &[StatusResult],
) -> String {
    let mut tokens = [identity.name, "integrations", "install", "skill"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if selection.source == SkillSelectionSource::All {
        tokens.push("--all-agents".to_owned());
    } else {
        for agent in &selection.agents {
            tokens.extend(["--agent".to_owned(), agent.id().to_owned()]);
        }
    }
    if project {
        tokens.push("--project".to_owned());
    }
    if results
        .iter()
        .any(|result| result.status == SkillInstallStatus::Modified)
    {
        tokens.push("--force".to_owned());
    }
    tokens.join(" ")
}

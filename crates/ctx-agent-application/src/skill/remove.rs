use anyhow::Result;
use ctx_agent_integrations::skill::{
    execute_remove, PathContext, SkillAgentSelection, SkillRemoveReceipt, SkillRemoveRequest,
    SkillTarget,
};

use crate::{IntegrationResultFact, IntegrationTelemetryFacts, ProductIdentity};

use super::install::selection_fact;

#[derive(Debug)]
pub struct SkillRemoveOutcome {
    pub receipt: SkillRemoveReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

pub fn remove(
    selection: SkillAgentSelection,
    project: bool,
    force: bool,
    context: &PathContext,
) -> Result<SkillRemoveOutcome> {
    let selection_fact = selection_fact(selection.source);
    let receipt = execute_remove(
        SkillRemoveRequest {
            selection,
            project,
            force,
        },
        context,
    )?;
    let telemetry = IntegrationTelemetryFacts {
        selection: Some(selection_fact),
        resolved_agents: Some(receipt.selection.agents.len()),
        result: Some(if receipt.failed == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        modified_targets: Some(receipt.removed_targets),
        ..IntegrationTelemetryFacts::default()
    };
    Ok(SkillRemoveOutcome { receipt, telemetry })
}

pub fn force_remove_command(identity: ProductIdentity<'_>, target: &SkillTarget) -> String {
    let project = if target.scope.as_str() == "project" {
        " --project"
    } else {
        ""
    };
    format!(
        "{} integrations remove skill --agent {}{project} --force",
        identity.name,
        target.agent.id()
    )
}

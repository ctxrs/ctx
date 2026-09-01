//! Native Agent Plugin lifecycle workflows independent of CLI presentation.

use ctx_agent_integrations::plugin::{
    execute_install, execute_remove, execute_status, PluginAgent, PluginInstallStatus,
    PluginReceipt, PluginRequest, PluginSelection,
};

use crate::{IntegrationResultFact, IntegrationTelemetryFacts, TargetSelectionFact};

#[derive(Debug, Clone)]
pub struct PluginApplicationRequest {
    pub agents: Vec<PluginAgent>,
    pub all_agents: bool,
    pub project: bool,
}

#[derive(Debug)]
pub struct PluginOutcome {
    pub receipt: PluginReceipt,
    pub telemetry: IntegrationTelemetryFacts,
}

pub fn install(
    request: PluginApplicationRequest,
    context: &ctx_agent_integrations::plugin::PluginContext,
) -> PluginOutcome {
    mutation_outcome(execute_install(domain_request(request), context))
}

pub fn status(
    request: PluginApplicationRequest,
    context: &ctx_agent_integrations::plugin::PluginContext,
) -> PluginOutcome {
    let receipt = execute_status(domain_request(request), context);
    let current = receipt
        .results
        .iter()
        .filter(|result| result.status.is_current())
        .count();
    let telemetry = IntegrationTelemetryFacts {
        selection: Some(selection_fact(receipt.selection)),
        resolved_agents: Some(receipt.results.len()),
        result: Some(
            if !receipt.results.is_empty() && current == receipt.results.len() {
                IntegrationResultFact::AllCurrent
            } else if current == 0 {
                IntegrationResultFact::NoneCurrent
            } else {
                IntegrationResultFact::PartiallyCurrent
            },
        ),
        current_targets: Some(current),
        missing_targets: Some(count_status(&receipt, PluginInstallStatus::Missing)),
        invalid_targets: Some(count_status(&receipt, PluginInstallStatus::Error)),
        unsupported_targets: Some(
            count_status(&receipt, PluginInstallStatus::ManualRequired)
                + count_status(&receipt, PluginInstallStatus::UnsupportedScope)
                + count_status(&receipt, PluginInstallStatus::CliMissing),
        ),
        ..IntegrationTelemetryFacts::default()
    };
    PluginOutcome { receipt, telemetry }
}

pub fn remove(
    request: PluginApplicationRequest,
    context: &ctx_agent_integrations::plugin::PluginContext,
) -> PluginOutcome {
    mutation_outcome(execute_remove(domain_request(request), context))
}

fn domain_request(request: PluginApplicationRequest) -> PluginRequest {
    PluginRequest {
        agents: request.agents,
        all_agents: request.all_agents,
        project: request.project,
    }
}

fn mutation_outcome(receipt: PluginReceipt) -> PluginOutcome {
    let telemetry = IntegrationTelemetryFacts {
        selection: Some(selection_fact(receipt.selection)),
        resolved_agents: Some(receipt.results.len()),
        result: Some(if receipt.failed == 0 {
            IntegrationResultFact::Ok
        } else {
            IntegrationResultFact::PartialError
        }),
        modified_targets: Some(receipt.modified),
        ..IntegrationTelemetryFacts::default()
    };
    PluginOutcome { receipt, telemetry }
}

fn selection_fact(selection: PluginSelection) -> TargetSelectionFact {
    match selection {
        PluginSelection::Detected => TargetSelectionFact::Detected,
        PluginSelection::Explicit => TargetSelectionFact::Explicit,
        PluginSelection::All => TargetSelectionFact::All,
    }
}

fn count_status(receipt: &PluginReceipt, status: PluginInstallStatus) -> usize {
    receipt
        .results
        .iter()
        .filter(|result| result.status == status)
        .count()
}

#[cfg(test)]
mod tests;

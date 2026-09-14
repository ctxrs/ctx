use std::path::PathBuf;

use ctx_agent_integrations::plugin::PluginContext;

use super::*;

#[test]
fn status_with_no_detected_targets_is_none_current() {
    let outcome = status(
        PluginApplicationRequest {
            agents: Vec::new(),
            all_agents: false,
            project: false,
        },
        &PluginContext::for_tests(PathBuf::from("/project"), None, None, None),
    );

    assert!(outcome.receipt.results.is_empty());
    assert_eq!(outcome.telemetry.resolved_agents, Some(0));
    assert_eq!(
        outcome.telemetry.result,
        Some(IntegrationResultFact::NoneCurrent)
    );
    assert_eq!(outcome.telemetry.current_targets, Some(0));
}

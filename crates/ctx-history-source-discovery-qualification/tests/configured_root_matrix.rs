mod support;

use std::collections::HashSet;

use ctx_history_source_discovery::{
    configured_root_capabilities, ConfiguredRootCapabilityState, ConfiguredRootExpander,
    ConfiguredRootPathKind,
};
use serde_json::{json, Value};

use support::provider_support_matrix;

fn path_kind(kind: ConfiguredRootPathKind) -> &'static str {
    match kind {
        ConfiguredRootPathKind::Directory => "directory",
        ConfiguredRootPathKind::File => "file",
    }
}

fn expander(expander: ConfiguredRootExpander) -> Value {
    match expander {
        ConfiguredRootExpander::ExactSource {
            source_format,
            route_role,
        } => json!({
            "kind": "exact_source",
            "source_format": source_format,
            "route_role": route_role,
        }),
        ConfiguredRootExpander::ClaudeHomeV1 => json!({"kind": "claude_home_v1"}),
        ConfiguredRootExpander::CodexHomeV1 => json!({"kind": "codex_home_v1"}),
        ConfiguredRootExpander::OpenClawStateRootV1 => {
            json!({"kind": "openclaw_state_root_v1"})
        }
        ConfiguredRootExpander::ClineCommonDataRootV1 => {
            json!({"kind": "cline_common_data_root_v1"})
        }
        ConfiguredRootExpander::OpenHandsKindV1 => json!({
            "kind": "openhands_kind_v1",
            "root_kinds": ["current-conversations", "legacy-persistence"],
        }),
    }
}

fn capability(state: ConfiguredRootCapabilityState) -> Value {
    match state {
        ConfiguredRootCapabilityState::Enabled {
            expected_path_kind,
            expander: configured_expander,
        } => json!({
            "state": "enabled",
            "expected_path_kind": path_kind(expected_path_kind),
            "expander": expander(configured_expander),
        }),
        ConfiguredRootCapabilityState::IntentionalAutomaticExact => {
            json!({"state": "intentional_automatic_exact"})
        }
        ConfiguredRootCapabilityState::PendingNamedSupport => {
            json!({"state": "pending_named_support"})
        }
    }
}

#[test]
fn public_matrix_matches_configured_root_implementation_exhaustively() {
    let matrix = provider_support_matrix();
    let rows = matrix["providers"].as_array().expect("provider rows");
    assert_eq!(rows.len(), configured_root_capabilities().len());
    let capture_providers = rows
        .iter()
        .map(|row| row["capture_provider"].as_str().expect("capture_provider"))
        .collect::<HashSet<_>>();
    assert_eq!(capture_providers.len(), rows.len());

    for configured in configured_root_capabilities() {
        let row = rows
            .iter()
            .find(|row| row["capture_provider"] == configured.provider.as_str())
            .unwrap_or_else(|| panic!("missing matrix row for {}", configured.provider.as_str()));
        assert_eq!(
            row["configured_root"],
            capability(configured.state),
            "configured-root matrix drift for {}",
            configured.provider.as_str(),
        );
    }
}

#[test]
fn qoder_documents_the_projects_container_as_its_complete_root() {
    let matrix = provider_support_matrix();
    let qoder = matrix["providers"]
        .as_array()
        .expect("provider rows")
        .iter()
        .find(|row| row["capture_provider"] == "qoder")
        .expect("Qoder matrix row");

    assert_eq!(
        qoder["history_locations"],
        json!([
            "Canonical complete root: `~/.qoder/projects`, which includes bounded per-project transcript and direct session JSONL leaves; selecting one project can include its transcript tree but not its direct session leaves."
        ]),
        "Qoder's projects container is required to include both native layouts",
    );
}

use super::types::{
    ProviderImportDependency, ProviderImportUnitGrouping, ProviderImportUnitOwner,
    ProviderImportUnitSpec,
};

const NO_DEPENDENCIES: &[ProviderImportDependency] = &[];
const SQLITE_DEPENDENCIES: &[ProviderImportDependency] =
    &[ProviderImportDependency::SqliteSidecars];
const MISTRAL_DEPENDENCIES: &[ProviderImportDependency] =
    &[ProviderImportDependency::SiblingFile("meta.json")];
const MUX_DEPENDENCIES: &[ProviderImportDependency] = &[
    ProviderImportDependency::SiblingFile("chat.jsonl"),
    ProviderImportDependency::SiblingFile("partial.json"),
    ProviderImportDependency::SiblingFile("metadata.json"),
];
const JUNIE_DEPENDENCIES: &[ProviderImportDependency] = &[ProviderImportDependency::AncestorFile {
    levels: 1,
    name: "index.jsonl",
}];
const KIMI_DEPENDENCIES: &[ProviderImportDependency] = &[
    ProviderImportDependency::AncestorFile {
        levels: 2,
        name: "state.json",
    },
    ProviderImportDependency::NearestAncestorFile("session_index.jsonl"),
];
const ROVODEV_DEPENDENCIES: &[ProviderImportDependency] =
    &[ProviderImportDependency::SiblingFile("metadata.json")];
const CONTINUE_DEPENDENCIES: &[ProviderImportDependency] =
    &[ProviderImportDependency::SiblingFile("sessions.json")];
const TRAE_DEPENDENCIES: &[ProviderImportDependency] = &[
    ProviderImportDependency::SqliteSidecars,
    ProviderImportDependency::SiblingFile("workspace.json"),
];

const JSONL_EXTENSIONS: &[&str] = &["jsonl"];
const JSON_EXTENSIONS: &[&str] = &["json"];
const NO_EXCLUDED_NAMES: &[&str] = &[];
const CONTINUE_EXCLUDED_NAMES: &[&str] = &["sessions.json"];
const MISTRAL_OWNER_NAMES: &[&str] = &["messages.jsonl"];
const MUX_OWNER_NAMES: &[&str] = &["chat.jsonl", "partial.json"];
const ROVODEV_OWNER_NAMES: &[&str] = &["session_context.json"];
const COPILOT_OWNER_NAMES: &[&str] = &["events.jsonl"];
const ANTIGRAVITY_OWNER_NAMES: &[&str] = &["transcript_full.jsonl", "transcript.jsonl"];
const KIMI_OWNER_NAMES: &[&str] = &["wire.jsonl"];
const JUNIE_OWNER_NAMES: &[&str] = &["events.jsonl"];
const TRAE_OWNER_NAMES: &[&str] = &["state.vscdb"];
const FIREBENDER_OWNER_NAMES: &[&str] = &["chat_history.db"];

const fn exact_file(dependencies: &'static [ProviderImportDependency]) -> ProviderImportUnitSpec {
    ProviderImportUnitSpec::PerFile {
        owner: ProviderImportUnitOwner::SourceFile,
        grouping: ProviderImportUnitGrouping::Each,
        dependencies,
    }
}

const fn named_files(
    names: &'static [&'static str],
    required_component: Option<&'static str>,
    grouping: ProviderImportUnitGrouping,
    dependencies: &'static [ProviderImportDependency],
) -> ProviderImportUnitSpec {
    ProviderImportUnitSpec::PerFile {
        owner: ProviderImportUnitOwner::FileNames {
            names,
            required_component,
        },
        grouping,
        dependencies,
    }
}

const fn extensions(
    values: &'static [&'static str],
    required_component: Option<&'static str>,
    excluded_names: &'static [&'static str],
) -> ProviderImportUnitSpec {
    ProviderImportUnitSpec::PerFile {
        owner: ProviderImportUnitOwner::Extensions {
            extensions: values,
            required_component,
            excluded_names,
        },
        grouping: ProviderImportUnitGrouping::Each,
        dependencies: NO_DEPENDENCIES,
    }
}

pub(super) fn provider_import_unit_spec(source_format: &str) -> ProviderImportUnitSpec {
    match source_format {
        "opencode_sqlite"
        | "kilo_sqlite"
        | "mimocode_sqlite"
        | "kiro_cli_sqlite"
        | "crush_sqlite"
        | "goose_sessions_sqlite"
        | "zed_threads_sqlite"
        | "forgecode_sqlite"
        | "deepagents_sessions_sqlite"
        | "lingma_sqlite"
        | "hermes_state_sqlite"
        | "astrbot_data_v4_sqlite"
        | "shelley_sqlite"
        | "warp_sqlite" => exact_file(SQLITE_DEPENDENCIES),
        "firebender_chat_history_sqlite" => named_files(
            FIREBENDER_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::Each,
            SQLITE_DEPENDENCIES,
        ),
        "trae_state_vscdb" => named_files(
            TRAE_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::Each,
            TRAE_DEPENDENCIES,
        ),
        "mistral_vibe_session_jsonl_tree" | "mistral_vibe_session_jsonl" => named_files(
            MISTRAL_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::Each,
            MISTRAL_DEPENDENCIES,
        ),
        "mux_session_jsonl_tree" | "mux_session_jsonl" => named_files(
            MUX_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::FirstPerDirectory,
            MUX_DEPENDENCIES,
        ),
        "junie_session_events_jsonl_tree" | "junie_session_events_jsonl" => named_files(
            JUNIE_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::Each,
            JUNIE_DEPENDENCIES,
        ),
        "kimi_code_cli_wire_jsonl_tree" | "kimi_code_cli_wire_jsonl" => named_files(
            KIMI_OWNER_NAMES,
            Some("agents"),
            ProviderImportUnitGrouping::Each,
            KIMI_DEPENDENCIES,
        ),
        "rovodev_session_json_tree" => named_files(
            ROVODEV_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::Each,
            ROVODEV_DEPENDENCIES,
        ),
        "continue_cli_sessions_json" => ProviderImportUnitSpec::PerFile {
            owner: ProviderImportUnitOwner::Extensions {
                extensions: JSON_EXTENSIONS,
                required_component: None,
                excluded_names: CONTINUE_EXCLUDED_NAMES,
            },
            grouping: ProviderImportUnitGrouping::Each,
            dependencies: CONTINUE_DEPENDENCIES,
        },
        "copilot_cli_session_events_jsonl" => named_files(
            COPILOT_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::Each,
            NO_DEPENDENCIES,
        ),
        "antigravity_cli_transcript_jsonl_tree" => named_files(
            ANTIGRAVITY_OWNER_NAMES,
            None,
            ProviderImportUnitGrouping::AntigravitySession,
            NO_DEPENDENCIES,
        ),
        "gemini_cli_chat_recording_jsonl" | "tabnine_cli_chat_recording_jsonl" => {
            extensions(JSONL_EXTENSIONS, Some("chats"), NO_EXCLUDED_NAMES)
        }
        "cursor_agent_transcript_jsonl_tree" | "cursor_agent_transcript_jsonl" => extensions(
            JSONL_EXTENSIONS,
            Some("agent-transcripts"),
            NO_EXCLUDED_NAMES,
        ),
        "qoder_transcript_jsonl_tree" | "qoder_transcript_jsonl" => {
            extensions(JSONL_EXTENSIONS, Some("transcript"), NO_EXCLUDED_NAMES)
        }
        "qwen_code_chat_jsonl_tree" | "qwen_code_chat_jsonl" => {
            extensions(JSONL_EXTENSIONS, Some("chats"), NO_EXCLUDED_NAMES)
        }
        "codebuddy_history_json" => ProviderImportUnitSpec::WholeSource,
        "codex_history_jsonl"
        | "codex_session_jsonl"
        | "pi_session_jsonl"
        | "claude_projects_jsonl_tree"
        | "factory_ai_droid_sessions_jsonl"
        | "windsurf_cascade_hook_transcript_jsonl_tree"
        | "windsurf_cascade_hook_transcript_jsonl" => {
            extensions(JSONL_EXTENSIONS, None, NO_EXCLUDED_NAMES)
        }
        "auggie_session_json" => extensions(JSON_EXTENSIONS, None, NO_EXCLUDED_NAMES),
        _ => ProviderImportUnitSpec::WholeSource,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sources::{discovery::provider_source_for_path, specs::PROVIDER_SPECS};
    use std::{collections::BTreeSet, path::PathBuf};

    const WHOLE_SOURCE_FORMATS: &[&str] = &[
        "cline_task_directory_json",
        "codebuddy_history_json",
        "codex_session_jsonl_tree",
        "nanoclaw_project",
        "openclaw_session_jsonl_tree",
        "openhands_file_events",
        "roo_task_directory_json",
    ];
    const REGISTERED_SQLITE_FORMATS: &[&str] = &[
        "opencode_sqlite",
        "kilo_sqlite",
        "mimocode_sqlite",
        "kiro_cli_sqlite",
        "crush_sqlite",
        "goose_sessions_sqlite",
        "zed_threads_sqlite",
        "forgecode_sqlite",
        "deepagents_sessions_sqlite",
        "lingma_sqlite",
        "trae_state_vscdb",
        "warp_sqlite",
        "hermes_state_sqlite",
        "astrbot_data_v4_sqlite",
        "shelley_sqlite",
        "firebender_chat_history_sqlite",
        "nanoclaw_project",
    ];

    #[test]
    fn every_registered_default_format_has_an_explicit_inventory_shape() {
        for spec in PROVIDER_SPECS {
            for location in spec.default_locations {
                let import_unit = provider_import_unit_spec(location.source_format);
                let expected_whole = WHOLE_SOURCE_FORMATS.contains(&location.source_format);
                assert_eq!(
                    matches!(import_unit, ProviderImportUnitSpec::WholeSource),
                    expected_whole,
                    "unexpected inventory shape for {} ({})",
                    spec.display_name,
                    location.source_format
                );
            }
        }
    }

    #[test]
    fn every_registered_sqlite_format_observes_sidecars_or_the_whole_root() {
        let mut registry_formats = BTreeSet::new();
        for spec in PROVIDER_SPECS {
            let explicit = provider_source_for_path(
                spec.provider,
                PathBuf::from(format!("missing-{}-history", spec.provider.as_str())),
            );
            if is_sqlite_format(explicit.source_format) {
                registry_formats.insert(explicit.source_format);
            }
            for location in spec.default_locations {
                if is_sqlite_format(location.source_format) {
                    registry_formats.insert(location.source_format);
                }
            }
        }
        assert_eq!(
            registry_formats,
            REGISTERED_SQLITE_FORMATS.iter().copied().collect(),
            "the SQLite source registry changed; classify its sidecar observation"
        );

        for &source_format in REGISTERED_SQLITE_FORMATS {
            match provider_import_unit_spec(source_format) {
                ProviderImportUnitSpec::PerFile { dependencies, .. } => assert!(
                    dependencies.contains(&ProviderImportDependency::SqliteSidecars),
                    "{source_format} does not observe SQLite sidecars"
                ),
                ProviderImportUnitSpec::WholeSource => assert_eq!(
                    source_format, "nanoclaw_project",
                    "only multi-database NanoClaw may use whole-root observation"
                ),
            }
        }
    }

    fn is_sqlite_format(source_format: &str) -> bool {
        source_format.contains("sqlite")
            || source_format.ends_with("vscdb")
            || source_format == "nanoclaw_project"
    }

    #[test]
    fn audited_companion_formats_declare_canonical_owners_and_dependencies() {
        assert_unit(
            "mistral_vibe_session_jsonl_tree",
            &["messages.jsonl"],
            ProviderImportUnitGrouping::Each,
            MISTRAL_DEPENDENCIES,
        );
        assert_unit(
            "mux_session_jsonl_tree",
            &["chat.jsonl", "partial.json"],
            ProviderImportUnitGrouping::FirstPerDirectory,
            MUX_DEPENDENCIES,
        );
        assert_unit(
            "junie_session_events_jsonl_tree",
            &["events.jsonl"],
            ProviderImportUnitGrouping::Each,
            JUNIE_DEPENDENCIES,
        );
        assert_unit(
            "kimi_code_cli_wire_jsonl_tree",
            &["wire.jsonl"],
            ProviderImportUnitGrouping::Each,
            KIMI_DEPENDENCIES,
        );
        assert_unit(
            "rovodev_session_json_tree",
            &["session_context.json"],
            ProviderImportUnitGrouping::Each,
            ROVODEV_DEPENDENCIES,
        );

        let ProviderImportUnitSpec::PerFile { dependencies, .. } =
            provider_import_unit_spec("continue_cli_sessions_json")
        else {
            panic!("Continue is not a manifested import unit");
        };
        assert_eq!(dependencies, CONTINUE_DEPENDENCIES);

        let ProviderImportUnitSpec::PerFile { dependencies, .. } =
            provider_import_unit_spec("trae_state_vscdb")
        else {
            panic!("Trae is not a manifested import unit");
        };
        assert_eq!(dependencies, TRAE_DEPENDENCIES);
    }

    fn assert_unit(
        source_format: &str,
        expected_names: &[&str],
        expected_grouping: ProviderImportUnitGrouping,
        expected_dependencies: &[ProviderImportDependency],
    ) {
        let ProviderImportUnitSpec::PerFile {
            owner: ProviderImportUnitOwner::FileNames { names, .. },
            grouping,
            dependencies,
        } = provider_import_unit_spec(source_format)
        else {
            panic!("{source_format} is not a named-file import unit");
        };
        assert_eq!(names, expected_names);
        assert_eq!(grouping, expected_grouping);
        assert_eq!(dependencies, expected_dependencies);
    }
}

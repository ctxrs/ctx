use super::*;
use ctx_history_core::SourceInventoryObservation;
use sha2::{Digest, Sha256};
use std::fmt;

/// Certifies the complete source membership produced by one provider route.
///
/// The authority and revision labels are persisted generation identity. Keep
/// them stable even though certification is no longer owned by the captured
/// route observation layer.
pub(crate) fn certify_source_inventory(
    route: &ProviderSource,
    certificates: &[CertifiedSource],
) -> SourceBackedRouteResult<CertifiedSourceInventory> {
    let path = route.path.as_os_str().as_encoded_bytes();
    let mut authority = Sha256::new();
    authority.update(b"ctx.captured-route-authority\0");
    authority.update((route.provider.as_str().len() as u64).to_be_bytes());
    authority.update(route.provider.as_str().as_bytes());
    authority.update((route.source_format.len() as u64).to_be_bytes());
    authority.update(route.source_format.as_bytes());
    authority.update((path.len() as u64).to_be_bytes());
    authority.update(path);

    let mut sources = certificates
        .iter()
        .map(|certificate| certificate.observation().source().clone())
        .collect::<Vec<_>>();
    sources.sort_by_key(SourceKey::exact_descriptor_digest);
    let mut revision = Sha256::new();
    revision.update(b"ctx.captured-route-inventory\0");
    revision.update((sources.len() as u64).to_be_bytes());
    for source in &sources {
        revision.update(source.exact_descriptor_digest());
    }
    let observation = SourceInventoryObservation::new(
        route.provider.as_str(),
        "ctx.captured-route",
        TypedKey::bytes(authority.finalize().to_vec()).map_err(source_inventory_contract)?,
        "ctx-captured-route-source-set-v1",
        revision.finalize().to_vec(),
    )
    .map_err(source_inventory_contract)?;
    CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "ctx-captured-route-inventory-v1",
        sources,
    )
    .map_err(source_inventory_contract)
}

fn source_inventory_contract(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

/// Whether a route was selected by provider discovery or supplied manually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteSelection {
    Automatic,
    ExplicitManual,
}

/// Provider-specific authority that must survive central registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedSelectorAuthority {
    DiscoveredWinner,
    ExplicitPath,
    CatalogLineage,
    ExactCwd,
    NamedSurface,
    SelectedWithRetainedExplicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteConstructor {
    ProviderSource,
    CatalogLineage,
    FiniteInventory,
    DiscoveryContext,
    ExactCwd,
    NamedSurface,
    SelectedWithRetainedRoutes,
}

/// Filesystem authority exposed by one landed route to provider-neutral
/// daemon watchers.
///
/// This is part of the central landed-route inventory, so watch derivation and
/// capture registration cannot silently disagree about whether a selected
/// path is an ordinary source root or a SQLite database family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedWatchTargetKind {
    Path,
    SqliteDatabase,
}

/// Static inventory of one landed provider/source-format registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBackedProviderRouteMetadata {
    pub provider: CaptureProvider,
    /// Format carried by the discovered or explicitly selected root.
    pub source_format: &'static str,
    /// Format carried by the `SourceKey`s certified by the adapter.
    pub certified_source_format: &'static str,
    pub automatic: bool,
    pub explicit_manual: bool,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<&'static str>,
    /// Provider-owned selector input required to construct this route.
    pub constructor: SourceBackedRouteConstructor,
    pub watch_target_kind: SourceBackedWatchTargetKind,
}

macro_rules! route {
    (
        $provider:ident, $format:literal, $automatic:literal, $explicit:literal,
        $authority:ident, $constructor:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $format,
            certified_source_format: $format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            unsupported_reason: None,
            constructor: SourceBackedRouteConstructor::$constructor,
            watch_target_kind: SourceBackedWatchTargetKind::Path,
        }
    };
    (
        $provider:ident, $selected_format:literal => $certified_format:literal,
        $automatic:literal, $explicit:literal, $authority:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $selected_format,
            certified_source_format: $certified_format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            unsupported_reason: None,
            constructor: SourceBackedRouteConstructor::ProviderSource,
            watch_target_kind: SourceBackedWatchTargetKind::Path,
        }
    };
    (
        $provider:ident, $format:literal, $automatic:literal, $explicit:literal,
        $authority:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $format,
            certified_source_format: $format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            unsupported_reason: None,
            constructor: SourceBackedRouteConstructor::ProviderSource,
            watch_target_kind: SourceBackedWatchTargetKind::Path,
        }
    };
}

macro_rules! sqlite_route {
    (
        $provider:ident, $format:literal, $automatic:literal, $explicit:literal,
        $authority:ident, $constructor:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $format,
            certified_source_format: $format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            unsupported_reason: None,
            constructor: SourceBackedRouteConstructor::$constructor,
            watch_target_kind: SourceBackedWatchTargetKind::SqliteDatabase,
        }
    };
    (
        $provider:ident, $format:literal, $automatic:literal, $explicit:literal,
        $authority:ident
    ) => {
        sqlite_route!(
            $provider,
            $format,
            $automatic,
            $explicit,
            $authority,
            ProviderSource
        )
    };
}

pub const fn source_backed_route_constructor(
    provider: CaptureProvider,
) -> Option<SourceBackedRouteConstructor> {
    let mut index = 0;
    while index < LANDED_SOURCE_BACKED_ROUTES.len() {
        let route = LANDED_SOURCE_BACKED_ROUTES[index];
        if route.provider as usize == provider as usize {
            return Some(route.constructor);
        }
        index += 1;
    }
    None
}

/// The central landed-adapter inventory. Adding a provider is deliberately a
/// data entry plus one private driver registration, not a new public trait.
pub const LANDED_SOURCE_BACKED_ROUTES: &[SourceBackedProviderRouteMetadata] = &[
    route!(
        Custom,
        "ctx_history_jsonl_v1",
        false,
        true,
        CatalogLineage,
        CatalogLineage
    ),
    route!(
        Codex,
        "codex_session_jsonl_tree" => "codex_session_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Codex,
        "codex_history_jsonl" => "codex_history_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Codex,
        "codex_session_jsonl" => "codex_session_jsonl",
        false,
        true,
        ExplicitPath
    ),
    route!(
        Claude,
        "claude_projects_jsonl_tree",
        true,
        true,
        DiscoveredWinner
    ),
    route!(Pi, "pi_session_jsonl", true, true, DiscoveredWinner),
    sqlite_route!(OpenCode, "opencode_sqlite", true, true, DiscoveredWinner),
    sqlite_route!(Kilo, "kilo_sqlite", true, true, DiscoveredWinner),
    sqlite_route!(KiroCli, "kiro_cli_sqlite", true, true, DiscoveredWinner),
    route!(
        Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Gemini,
        "gemini_cli_chat_recording_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Tabnine,
        "tabnine_cli_chat_recording_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Cursor,
        "cursor_agent_transcript_jsonl_tree" => "cursor_agent_transcript_jsonl_tree",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Cursor,
        "cursor_agent_transcript_jsonl" => "cursor_agent_transcript_jsonl_tree",
        false,
        true,
        ExplicitPath
    ),
    route!(
        Windsurf,
        "windsurf_cascade_hook_transcript_jsonl_tree" => "windsurf_cascade_hook_transcript_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Windsurf,
        "windsurf_cascade_hook_transcript_jsonl" => "windsurf_cascade_hook_transcript_jsonl",
        false,
        true,
        ExplicitPath
    ),
    sqlite_route!(Zed, "zed_threads_sqlite", true, true, DiscoveredWinner),
    route!(
        CopilotCli,
        "copilot_cli_session_events_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        FactoryAiDroid,
        "factory_ai_droid_sessions_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        QwenCode,
        "qwen_code_chat_jsonl_tree" => "qwen_code_chat_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        QwenCode,
        "qwen_code_chat_jsonl" => "qwen_code_chat_jsonl",
        false,
        true,
        ExplicitPath
    ),
    route!(
        KimiCodeCli,
        "kimi_code_cli_wire_jsonl_tree" => "kimi_code_cli_wire_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        KimiCodeCli,
        "kimi_code_cli_wire_jsonl" => "kimi_code_cli_wire_jsonl",
        false,
        true,
        ExplicitPath
    ),
    route!(Auggie, "auggie_session_json", true, true, DiscoveredWinner),
    route!(
        Junie,
        "junie_session_events_jsonl_tree" => "junie_session_events_jsonl_tree",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Junie,
        "junie_session_events_jsonl" => "junie_session_events_jsonl_tree",
        false,
        true,
        ExplicitPath
    ),
    sqlite_route!(
        Firebender,
        "firebender_chat_history_sqlite",
        true,
        true,
        DiscoveredWinner
    ),
    sqlite_route!(
        ForgeCode,
        "forgecode_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit
    ),
    sqlite_route!(
        DeepAgents,
        "deepagents_sessions_sqlite",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        MistralVibe,
        "mistral_vibe_session_jsonl_tree" => "mistral_vibe_session_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        MistralVibe,
        "mistral_vibe_session_jsonl" => "mistral_vibe_session_jsonl",
        false,
        true,
        ExplicitPath
    ),
    route!(
        Mux,
        "mux_session_jsonl_tree" => "mux_session_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Mux,
        "mux_session_jsonl" => "mux_session_jsonl",
        false,
        true,
        ExplicitPath
    ),
    route!(
        RovoDev,
        "rovodev_session_json_tree",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        OpenClaw,
        "openclaw_session_jsonl_tree",
        true,
        true,
        DiscoveredWinner
    ),
    sqlite_route!(Hermes, "hermes_state_sqlite", true, true, DiscoveredWinner),
    route!(
        NanoClaw,
        "nanoclaw_project",
        true,
        true,
        CatalogLineage,
        CatalogLineage
    ),
    sqlite_route!(
        AstrBot,
        "astrbot_data_v4_sqlite",
        true,
        true,
        DiscoveredWinner,
        DiscoveryContext
    ),
    sqlite_route!(Shelley, "shelley_sqlite", true, false, ExactCwd, ExactCwd),
    route!(
        Continue,
        "continue_cli_sessions_json",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        OpenHands,
        "openhands_file_events",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Cline,
        "cline_task_directory_json",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        RooCode,
        "roo_task_directory_json",
        true,
        true,
        DiscoveredWinner
    ),
    sqlite_route!(
        Crush,
        "crush_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        FiniteInventory
    ),
    sqlite_route!(
        Goose,
        "goose_sessions_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        SelectedWithRetainedRoutes
    ),
    sqlite_route!(
        Lingma,
        "lingma_sqlite",
        true,
        true,
        DiscoveredWinner,
        FiniteInventory
    ),
    route!(
        Qoder,
        "qoder_transcript_jsonl_tree" => "qoder_transcript_jsonl",
        true,
        true,
        DiscoveredWinner
    ),
    route!(
        Qoder,
        "qoder_transcript_jsonl" => "qoder_transcript_jsonl",
        false,
        true,
        ExplicitPath
    ),
    sqlite_route!(Warp, "warp_sqlite", true, true, NamedSurface, NamedSurface),
    route!(
        CodeBuddy,
        "codebuddy_history_json",
        true,
        true,
        DiscoveredWinner
    ),
    sqlite_route!(Trae, "trae_state_vscdb", true, true, DiscoveredWinner),
    sqlite_route!(MiMoCode, "mimocode_sqlite", true, true, DiscoveredWinner),
];

pub fn source_backed_route_inventory() -> &'static [SourceBackedProviderRouteMetadata] {
    LANDED_SOURCE_BACKED_ROUTES
}

#[cfg(test)]
fn registry_inventory_oracle() -> String {
    use std::fmt::Write;

    let mut rows = String::new();
    for route in LANDED_SOURCE_BACKED_ROUTES {
        writeln!(
            rows,
            "{:?}|{}|{}|{}|{}|{:?}|{}|{:?}",
            route.provider,
            route.source_format,
            route.certified_source_format,
            route.automatic,
            route.explicit_manual,
            route.selector_authority,
            route.unsupported_reason.unwrap_or("none"),
            route.constructor,
        )
        .expect("writing a registry inventory row to String cannot fail");
    }
    rows
}

#[cfg(test)]
mod oracle_tests {
    use super::*;
    use ctx_history_core::SourceObservation;
    use std::path::PathBuf;

    const BASELINE_REGISTRY_INVENTORY: &str = "\
Custom|ctx_history_jsonl_v1|ctx_history_jsonl_v1|false|true|CatalogLineage|none|CatalogLineage
Codex|codex_session_jsonl_tree|codex_session_jsonl|true|true|DiscoveredWinner|none|ProviderSource
Codex|codex_history_jsonl|codex_history_jsonl|true|true|DiscoveredWinner|none|ProviderSource
Codex|codex_session_jsonl|codex_session_jsonl|false|true|ExplicitPath|none|ProviderSource
Claude|claude_projects_jsonl_tree|claude_projects_jsonl_tree|true|true|DiscoveredWinner|none|ProviderSource
Pi|pi_session_jsonl|pi_session_jsonl|true|true|DiscoveredWinner|none|ProviderSource
OpenCode|opencode_sqlite|opencode_sqlite|true|true|DiscoveredWinner|none|ProviderSource
Kilo|kilo_sqlite|kilo_sqlite|true|true|DiscoveredWinner|none|ProviderSource
KiroCli|kiro_cli_sqlite|kiro_cli_sqlite|true|true|DiscoveredWinner|none|ProviderSource
Antigravity|antigravity_cli_transcript_jsonl_tree|antigravity_cli_transcript_jsonl_tree|true|true|DiscoveredWinner|none|ProviderSource
Gemini|gemini_cli_chat_recording_jsonl|gemini_cli_chat_recording_jsonl|true|true|DiscoveredWinner|none|ProviderSource
Tabnine|tabnine_cli_chat_recording_jsonl|tabnine_cli_chat_recording_jsonl|true|true|DiscoveredWinner|none|ProviderSource
Cursor|cursor_agent_transcript_jsonl_tree|cursor_agent_transcript_jsonl_tree|true|true|DiscoveredWinner|none|ProviderSource
Cursor|cursor_agent_transcript_jsonl|cursor_agent_transcript_jsonl_tree|false|true|ExplicitPath|none|ProviderSource
Windsurf|windsurf_cascade_hook_transcript_jsonl_tree|windsurf_cascade_hook_transcript_jsonl|true|true|DiscoveredWinner|none|ProviderSource
Windsurf|windsurf_cascade_hook_transcript_jsonl|windsurf_cascade_hook_transcript_jsonl|false|true|ExplicitPath|none|ProviderSource
Zed|zed_threads_sqlite|zed_threads_sqlite|true|true|DiscoveredWinner|none|ProviderSource
CopilotCli|copilot_cli_session_events_jsonl|copilot_cli_session_events_jsonl|true|true|DiscoveredWinner|none|ProviderSource
FactoryAiDroid|factory_ai_droid_sessions_jsonl|factory_ai_droid_sessions_jsonl|true|true|DiscoveredWinner|none|ProviderSource
QwenCode|qwen_code_chat_jsonl_tree|qwen_code_chat_jsonl|true|true|DiscoveredWinner|none|ProviderSource
QwenCode|qwen_code_chat_jsonl|qwen_code_chat_jsonl|false|true|ExplicitPath|none|ProviderSource
KimiCodeCli|kimi_code_cli_wire_jsonl_tree|kimi_code_cli_wire_jsonl|true|true|DiscoveredWinner|none|ProviderSource
KimiCodeCli|kimi_code_cli_wire_jsonl|kimi_code_cli_wire_jsonl|false|true|ExplicitPath|none|ProviderSource
Auggie|auggie_session_json|auggie_session_json|true|true|DiscoveredWinner|none|ProviderSource
Junie|junie_session_events_jsonl_tree|junie_session_events_jsonl_tree|true|true|DiscoveredWinner|none|ProviderSource
Junie|junie_session_events_jsonl|junie_session_events_jsonl_tree|false|true|ExplicitPath|none|ProviderSource
Firebender|firebender_chat_history_sqlite|firebender_chat_history_sqlite|true|true|DiscoveredWinner|none|ProviderSource
ForgeCode|forgecode_sqlite|forgecode_sqlite|true|true|SelectedWithRetainedExplicit|none|ProviderSource
DeepAgents|deepagents_sessions_sqlite|deepagents_sessions_sqlite|true|true|DiscoveredWinner|none|ProviderSource
MistralVibe|mistral_vibe_session_jsonl_tree|mistral_vibe_session_jsonl|true|true|DiscoveredWinner|none|ProviderSource
MistralVibe|mistral_vibe_session_jsonl|mistral_vibe_session_jsonl|false|true|ExplicitPath|none|ProviderSource
Mux|mux_session_jsonl_tree|mux_session_jsonl|true|true|DiscoveredWinner|none|ProviderSource
Mux|mux_session_jsonl|mux_session_jsonl|false|true|ExplicitPath|none|ProviderSource
RovoDev|rovodev_session_json_tree|rovodev_session_json_tree|true|true|DiscoveredWinner|none|ProviderSource
OpenClaw|openclaw_session_jsonl_tree|openclaw_session_jsonl_tree|true|true|DiscoveredWinner|none|ProviderSource
Hermes|hermes_state_sqlite|hermes_state_sqlite|true|true|DiscoveredWinner|none|ProviderSource
NanoClaw|nanoclaw_project|nanoclaw_project|true|true|CatalogLineage|none|CatalogLineage
AstrBot|astrbot_data_v4_sqlite|astrbot_data_v4_sqlite|true|true|DiscoveredWinner|none|DiscoveryContext
Shelley|shelley_sqlite|shelley_sqlite|true|false|ExactCwd|none|ExactCwd
Continue|continue_cli_sessions_json|continue_cli_sessions_json|true|true|DiscoveredWinner|none|ProviderSource
OpenHands|openhands_file_events|openhands_file_events|true|true|DiscoveredWinner|none|ProviderSource
Cline|cline_task_directory_json|cline_task_directory_json|true|true|DiscoveredWinner|none|ProviderSource
RooCode|roo_task_directory_json|roo_task_directory_json|true|true|DiscoveredWinner|none|ProviderSource
Crush|crush_sqlite|crush_sqlite|true|true|SelectedWithRetainedExplicit|none|FiniteInventory
Goose|goose_sessions_sqlite|goose_sessions_sqlite|true|true|SelectedWithRetainedExplicit|none|SelectedWithRetainedRoutes
Lingma|lingma_sqlite|lingma_sqlite|true|true|DiscoveredWinner|none|FiniteInventory
Qoder|qoder_transcript_jsonl_tree|qoder_transcript_jsonl|true|true|DiscoveredWinner|none|ProviderSource
Qoder|qoder_transcript_jsonl|qoder_transcript_jsonl|false|true|ExplicitPath|none|ProviderSource
Warp|warp_sqlite|warp_sqlite|true|true|NamedSurface|none|NamedSurface
CodeBuddy|codebuddy_history_json|codebuddy_history_json|true|true|DiscoveredWinner|none|ProviderSource
Trae|trae_state_vscdb|trae_state_vscdb|true|true|DiscoveredWinner|none|ProviderSource
MiMoCode|mimocode_sqlite|mimocode_sqlite|true|true|DiscoveredWinner|none|ProviderSource
";

    #[test]
    fn landed_registry_inventory_matches_pre_shard_oracle() {
        assert_eq!(LANDED_SOURCE_BACKED_ROUTES.len(), 52);
        assert_eq!(
            LANDED_SOURCE_BACKED_ROUTES
                .iter()
                .filter(|route| route.automatic)
                .count(),
            42
        );
        assert_eq!(registry_inventory_oracle(), BASELINE_REGISTRY_INVENTORY);
    }

    #[test]
    fn neutral_inventory_certification_preserves_route_inventory_identity() {
        let route = fixture_inventory_route("/fixture/.codex/history.jsonl");
        let first = fixture_inventory_certificate(1);
        let second = fixture_inventory_certificate(2);

        let forward = certify_source_inventory(&route, &[first.clone(), second.clone()]).unwrap();
        let reversed = certify_source_inventory(&route, &[second.clone(), first.clone()]).unwrap();

        assert_eq!(forward, reversed);
        assert_eq!(
            forward.observation().provider(),
            CaptureProvider::Codex.as_str()
        );
        assert_eq!(
            forward.observation().authority_namespace(),
            "ctx.captured-route"
        );
        assert_eq!(
            forward.observation().revision_kind(),
            "ctx-captured-route-source-set-v1"
        );
        assert_eq!(
            forward.discovery_revision(),
            "ctx-captured-route-inventory-v1"
        );
        assert_eq!(forward.observed_sources(), 2);
        assert!(forward.contains(first.observation().source()));
        assert!(forward.contains(second.observation().source()));

        let other_route = fixture_inventory_route("/fixture/.codex/other-history.jsonl");
        assert_ne!(
            certify_source_inventory(&other_route, &[first.clone(), second]).unwrap(),
            forward
        );

        let duplicate = certify_source_inventory(&route, &[first.clone(), first]).unwrap_err();
        assert_eq!(duplicate.kind, SourceBackedRouteErrorKind::Internal);
    }

    fn fixture_inventory_route(path: &str) -> ProviderSource {
        ProviderSource {
            provider: CaptureProvider::Codex,
            path: PathBuf::from(path),
            exists: true,
            source_format: "codex_history_jsonl",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: crate::ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        }
    }

    fn fixture_inventory_certificate(lineage: u8) -> CertifiedSource {
        let source = SourceKey::derive(
            CaptureProvider::Codex.as_str(),
            "codex_history_jsonl",
            "inventory-certification-test-v1",
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap();
        let observation =
            SourceObservation::new(source, "fixture-revision", vec![lineage]).unwrap();
        CertifiedSource::certify(
            observation.clone(),
            observation,
            "inventory-certification-test-v1",
            [lineage; 32],
            ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                indexed_documents: 1,
                certified_bytes: 1,
                ..ScannedSourceCounts::default()
            },
        )
        .unwrap()
    }
}

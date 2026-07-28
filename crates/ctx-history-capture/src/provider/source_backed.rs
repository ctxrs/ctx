//! Shared coordination for provider-owned source-backed projections.
//!
//! Provider adapters remain responsible for discovery, parsing, projection,
//! and exact source resolution. This module owns the one production
//! publication boundary: all registered adapters stage into one
//! [`GenerationWriter`] and no adapter can publish a generation by itself.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
    CertifiedSourceInventory, ContentSourceResolver, EventHydrationRequest, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, ScannedSourceCounts, SessionHydrationRequest,
    SourceAnchor, SourceFrontier, SourceKey, TypedKey,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, LexicalDocument, RevalidationTarget, WriterOptions,
};
use thiserror::Error;

use super::codex::nativepath::{
    codex_source_observation, codex_writer_base_sources, discover_codex_root_inventory_v0,
    ingest_codex_sources_serial_v0, managed_codex_session_source, CodexLocatorResolverV0,
    CodexSourceBackedCountersV0, CodexSourceBackedPhaseTimingsV0,
};
use super::custom_history_jsonl::{
    observe_custom_history_source_backed_explicit, revalidate_custom_history_source_backed,
    scan_custom_history_source_backed_explicit, CustomHistorySourceBackedDisposition,
    CustomHistorySourceBackedInput, CustomHistorySourceBackedOutcome,
    CustomHistorySourceBackedResolver,
};
pub use super::providers::crush::native_path::source_backed::{
    CrushProjectDatabaseV0, CrushProjectInventoryObservationV0, CrushProjectInventorySourceV0,
};
use super::providers::{
    astrbot::native_path::source_backed::{
        scan_astrbot_source_backed_v0, AstrBotSourceBackedInventoryV0,
        AstrBotSourceBackedResolverV0,
    },
    auggie::native_path::source_backed::{
        discover_auggie_source_backed, hydrate_auggie_source_backed,
        project_auggie_source_backed_inventory, AuggieSourceBackedRoot,
    },
    claude::nativepath::source_backed::{
        discover_claude_source_backed, hydrate_claude_source_record, ClaudeSourceBackedScanner,
    },
    codebuddy::native_path::{
        hydrate_codebuddy_source_backed_record, scan_codebuddy_source_backed_root,
    },
    continue_cli::native_path::{
        discover_continue_root, hydrate_continue_source_backed_record, ContinueSourceBackedOutcome,
        ContinueSourceBackedReader,
    },
    crush::native_path::source_backed::{
        bind_inventory as bind_crush_inventory, closing_observation as closing_crush_observation,
        exact_replay_matches as crush_exact_replay_matches,
        finish_opened_source as finish_crush_source, open_source as open_crush_source,
        scan_source as scan_crush_source, CrushLocatorResolverV0, CRUSH_DISCOVERY_REVISION,
        CRUSH_FRONTIER_KIND, CRUSH_PARSER_REVISION, CRUSH_SOURCE_SCHEMA_VARIANT,
    },
    cursor::{
        extract_cursor_source_backed_cold, hydrate_cursor_source_backed_message,
        CursorSourceBackedPage, CursorSourceBackedRecord, CursorSourceBackedSink,
        CursorSourceBackedSourcePlan, CursorSourceBackedTerminal,
    },
    deepagents::native_path::source_backed::{
        DeepAgentsDatabaseSelectionV0, DeepAgentsLocatorResolverV0, DeepAgentsSourceBackedScannerV0,
    },
    firebender::native_path::{
        hydrate_firebender_source_backed_row, prepare_firebender_source_backed,
        FirebenderSourceBackedPlan,
    },
    forgecode::nativepath::source_backed::{
        open_forgecode_source_backed_v0, ForgeCodeSourceBackedDiscoveryV0,
        ForgeCodeSourceBackedResolverV0, ForgeCodeSourceSelectionV0,
    },
    gemini::nativepath::{
        discover_gemini_transcripts, hydrate_gemini_source_backed_record,
        GeminiSourceBackedLeafReader,
    },
    goose::{
        GooseSourceBackedAdapterV0, GooseSourceBackedResolverV0, GooseSourceBackedSelectionV0,
        GooseSourceRouteV0,
    },
    hermes::source_backed::{
        hermes_source_backed_explicit, hydrate_hermes_source_backed_message,
        scan_hermes_source_backed, HermesSourceBackedError, HermesSourceBackedRecord,
        HermesSourceCandidate,
    },
    junie::nativepath::{
        JunieLocatorResolverV0, JunieSourceBackedEmissionV0, JunieSourceBackedScannerV0,
    },
    kimi::native_path::source_backed::{KimiSourceBackedCatalog, KimiSourceBackedResolver},
    kiro::native_path::{scan_kiro_source_backed_v0, KiroLocatorResolverV0},
    lingma::{
        scan_lingma_source_backed_v0, LingmaDatabaseSourceV0, LingmaExactContentFailureKindV0,
        LingmaSourceBackedResolverV0, LingmaSourceInventoryV0,
    },
    mistral_vibe::native_path::source_backed::scan_mistral_vibe_source_backed,
    mux::native_path::{
        discover_mux_source_backed_sources, scan_mux_source_backed, MuxSourceBackedDisposition,
    },
    nanoclaw::native_path::source_backed::{
        hydrate_nanoclaw_source_backed_exact, nanoclaw_source_key, scan_nanoclaw_source_backed,
    },
    native_jsonl::native_path::{
        antigravity_source_backed_adapter, copilot_source_backed_adapter,
        factory_droid_source_backed_adapter, qoder_source_backed_adapter,
        qwen_code_source_backed_adapter, tabnine_source_backed_adapter,
        windsurf_source_backed_adapter, DirectJsonlCertifiedLeaf, DirectJsonlSourceAdapter,
    },
    openclaw::openclaw_source_backed_adapter_v0,
    opencode::native_path::source_backed::{
        kilo_source_backed_registration, mimocode_source_backed_registration,
        opencode_source_backed_registration, OpenCodeSourceBackedError,
    },
    openhands::nativepath::{OpenHandsLocatorResolverV1, OpenHandsSourceBackedAdapterV1},
    pi::nativepath::{
        project_pi_source_backed_root_cold, PiSourceBackedResolver, PiSourceBackedRoot,
    },
    rovodev::native_path::{
        discover_rovodev_source_backed, hydrate_rovodev_source_record,
        RovoDevSourceBackedDisposition, RovoDevSourceBackedReader,
    },
    shelley::native_path::source_backed::{
        discover_shelley_source_backed_exact_cwd, ShelleySourceBackedAdapter,
    },
    task_json::cline_nativepath::{
        cline_task_json_source_backed_adapter, cline_task_json_source_backed_resolver,
        roo_task_json_source_backed_adapter, roo_task_json_source_backed_resolver,
    },
    trae::nativepath::{
        hydrate_trae_source_backed_locator_v0, scan_trae_source_backed_explicit_v0,
        TraeSourceBackedErrorV0,
    },
    warp::{project_warp_source_backed_v0, resolve_warp_locator_v0, WarpSourceSelectionV0},
    zed::native_path::source_backed::{
        acquire_snapshot as acquire_zed_snapshot, decode_sha256_hex as decode_zed_digest,
        scan_zed_native_snapshot, snapshot_revision_digest as zed_snapshot_revision_digest,
        source_observation as zed_source_observation, zed_source_key, ZedLocatorResolverV0,
        ZedSourceBackedSinkV0,
    },
};
use crate::{
    discover_provider_sources_with_context, CaptureError, DiscoveryContext, DiscoveryIssue,
    DiscoveryPlatform, ProviderAdapterContext, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceStatus, Result as CaptureResult,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, GEMINI_CLI_SOURCE_FORMAT,
};

pub type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
pub type SourceBackedRouteResult<T> = Result<T, SourceBackedRouteError>;

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

/// Exact hydration coverage advertised by a landed adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedHydrationSupport {
    Full,
    Partial,
    Unsupported,
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
    pub exact_hydration: SourceBackedHydrationSupport,
    pub hydration_limitation: Option<&'static str>,
    pub unsupported_reason: Option<&'static str>,
}

macro_rules! route {
    (
        $provider:ident, $selected_format:literal => $certified_format:literal,
        $automatic:literal, $explicit:literal, $authority:ident, $hydration:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $selected_format,
            certified_source_format: $certified_format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            exact_hydration: SourceBackedHydrationSupport::$hydration,
            hydration_limitation: None,
            unsupported_reason: None,
        }
    };
    (
        $provider:ident, $format:literal, $automatic:literal, $explicit:literal,
        $authority:ident, $hydration:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $format,
            certified_source_format: $format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            exact_hydration: SourceBackedHydrationSupport::$hydration,
            hydration_limitation: None,
            unsupported_reason: None,
        }
    };
}

macro_rules! partial_route {
    (
        $provider:ident, $selected_format:literal => $certified_format:literal,
        $automatic:literal, $explicit:literal, $authority:ident, $reason:literal
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $selected_format,
            certified_source_format: $certified_format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            exact_hydration: SourceBackedHydrationSupport::Partial,
            hydration_limitation: Some($reason),
            unsupported_reason: None,
        }
    };
    (
        $provider:ident, $format:literal, $automatic:literal, $explicit:literal,
        $authority:ident, $reason:literal
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $format,
            certified_source_format: $format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            exact_hydration: SourceBackedHydrationSupport::Partial,
            hydration_limitation: Some($reason),
            unsupported_reason: None,
        }
    };
}

macro_rules! unsupported_format_route {
    (
        $provider:ident, $selected_format:literal => $certified_format:literal,
        $automatic:literal, $explicit:literal, $authority:ident, $reason:literal
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $selected_format,
            certified_source_format: $certified_format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            exact_hydration: SourceBackedHydrationSupport::Unsupported,
            hydration_limitation: None,
            unsupported_reason: Some($reason),
        }
    };
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

pub const fn source_backed_route_constructor(
    provider: CaptureProvider,
) -> Option<SourceBackedRouteConstructor> {
    Some(match provider {
        CaptureProvider::Custom | CaptureProvider::NanoClaw => {
            SourceBackedRouteConstructor::CatalogLineage
        }
        CaptureProvider::Crush | CaptureProvider::Lingma => {
            SourceBackedRouteConstructor::FiniteInventory
        }
        CaptureProvider::AstrBot => SourceBackedRouteConstructor::DiscoveryContext,
        CaptureProvider::Shelley => SourceBackedRouteConstructor::ExactCwd,
        CaptureProvider::Warp => SourceBackedRouteConstructor::NamedSurface,
        CaptureProvider::Goose => SourceBackedRouteConstructor::SelectedWithRetainedRoutes,
        CaptureProvider::Codex
        | CaptureProvider::Claude
        | CaptureProvider::Pi
        | CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::KiroCli
        | CaptureProvider::Antigravity
        | CaptureProvider::Gemini
        | CaptureProvider::Tabnine
        | CaptureProvider::Cursor
        | CaptureProvider::Windsurf
        | CaptureProvider::Zed
        | CaptureProvider::CopilotCli
        | CaptureProvider::FactoryAiDroid
        | CaptureProvider::QwenCode
        | CaptureProvider::KimiCodeCli
        | CaptureProvider::Auggie
        | CaptureProvider::Junie
        | CaptureProvider::Firebender
        | CaptureProvider::ForgeCode
        | CaptureProvider::DeepAgents
        | CaptureProvider::MistralVibe
        | CaptureProvider::Mux
        | CaptureProvider::RovoDev
        | CaptureProvider::OpenClaw
        | CaptureProvider::Hermes
        | CaptureProvider::Continue
        | CaptureProvider::OpenHands
        | CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::Qoder
        | CaptureProvider::CodeBuddy
        | CaptureProvider::Trae
        | CaptureProvider::MiMoCode => SourceBackedRouteConstructor::ProviderSource,
        _ => return None,
    })
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
        Full
    ),
    route!(
        Codex,
        "codex_session_jsonl_tree" => "codex_session_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    unsupported_format_route!(
        Codex,
        "codex_history_jsonl" => "codex_history_jsonl",
        true,
        true,
        DiscoveredWinner,
        "Codex prompt-history source-backed scan/certification and exact JSONL hydration are not exposed to the coordinator"
    ),
    unsupported_format_route!(
        Codex,
        "codex_session_jsonl" => "codex_session_jsonl",
        false,
        true,
        ExplicitPath,
        "single-file Codex rollout source-backed discovery is not exposed to the coordinator"
    ),
    route!(Claude, "claude_projects_jsonl_tree", true, true, DiscoveredWinner, Full),
    route!(Pi, "pi_session_jsonl", true, true, DiscoveredWinner, Full),
    route!(OpenCode, "opencode_sqlite", true, true, DiscoveredWinner, Full),
    route!(Kilo, "kilo_sqlite", true, true, DiscoveredWinner, Full),
    route!(KiroCli, "kiro_cli_sqlite", true, true, DiscoveredWinner, Full),
    route!(
        Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Gemini,
        "gemini_cli_chat_recording_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Tabnine,
        "tabnine_cli_chat_recording_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Cursor,
        "cursor_agent_transcript_jsonl_tree" => "cursor_agent_transcript_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Cursor,
        "cursor_agent_transcript_jsonl" => "cursor_agent_transcript_jsonl_tree",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        Windsurf,
        "windsurf_cascade_hook_transcript_jsonl_tree" => "windsurf_cascade_hook_transcript_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Windsurf,
        "windsurf_cascade_hook_transcript_jsonl" => "windsurf_cascade_hook_transcript_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(Zed, "zed_threads_sqlite", true, true, DiscoveredWinner, Full),
    route!(
        CopilotCli,
        "copilot_cli_session_events_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        FactoryAiDroid,
        "factory_ai_droid_sessions_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        QwenCode,
        "qwen_code_chat_jsonl_tree" => "qwen_code_chat_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        QwenCode,
        "qwen_code_chat_jsonl" => "qwen_code_chat_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        KimiCodeCli,
        "kimi_code_cli_wire_jsonl_tree" => "kimi_code_cli_wire_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        KimiCodeCli,
        "kimi_code_cli_wire_jsonl" => "kimi_code_cli_wire_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(Auggie, "auggie_session_json", true, true, DiscoveredWinner, Full),
    partial_route!(
        Junie,
        "junie_session_events_jsonl_tree" => "junie_session_events_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        "Junie exact hydration is limited to message records with provider-owned JSONL addresses"
    ),
    partial_route!(
        Junie,
        "junie_session_events_jsonl" => "junie_session_events_jsonl_tree",
        false,
        true,
        ExplicitPath,
        "Junie exact hydration is limited to message records with provider-owned JSONL addresses"
    ),
    route!(
        Firebender,
        "firebender_chat_history_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        ForgeCode,
        "forgecode_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        Full
    ),
    route!(
        DeepAgents,
        "deepagents_sessions_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        MistralVibe,
        "mistral_vibe_session_jsonl_tree" => "mistral_vibe_session_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        MistralVibe,
        "mistral_vibe_session_jsonl" => "mistral_vibe_session_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    partial_route!(
        Mux,
        "mux_session_jsonl_tree" => "mux_session_jsonl",
        true,
        true,
        DiscoveredWinner,
        "Mux exact hydration requires the brokered compound-file content route"
    ),
    partial_route!(
        Mux,
        "mux_session_jsonl" => "mux_session_jsonl",
        false,
        true,
        ExplicitPath,
        "Mux exact hydration requires the brokered compound-file content route"
    ),
    route!(
        RovoDev,
        "rovodev_session_json_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        OpenClaw,
        "openclaw_session_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(Hermes, "hermes_state_sqlite", true, true, DiscoveredWinner, Full),
    route!(
        NanoClaw,
        "nanoclaw_project",
        false,
        true,
        CatalogLineage,
        Full
    ),
    partial_route!(
        AstrBot,
        "astrbot_data_v4_sqlite",
        true,
        true,
        DiscoveredWinner,
        "AstrBot exact hydration is currently limited to conversation messages"
    ),
    route!(Shelley, "shelley_sqlite", true, false, ExactCwd, Full),
    route!(
        Continue,
        "continue_cli_sessions_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        OpenHands,
        "openhands_file_events",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Cline,
        "cline_task_directory_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        RooCode,
        "roo_task_directory_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Crush,
        "crush_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        Full
    ),
    route!(
        Goose,
        "goose_sessions_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        Full
    ),
    partial_route!(
        Lingma,
        "lingma_sqlite",
        true,
        true,
        DiscoveredWinner,
        "Lingma exact hydration is available for row-local user prompts; assistant records remain preview-only"
    ),
    route!(
        Qoder,
        "qoder_transcript_jsonl_tree" => "qoder_transcript_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Qoder,
        "qoder_transcript_jsonl" => "qoder_transcript_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(Warp, "warp_sqlite", true, true, NamedSurface, Full),
    route!(
        CodeBuddy,
        "codebuddy_history_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(Trae, "trae_state_vscdb", true, true, ExplicitPath, Full),
    route!(MiMoCode, "mimocode_sqlite", true, true, DiscoveredWinner, Full),
];

pub fn source_backed_route_inventory() -> &'static [SourceBackedProviderRouteMetadata] {
    LANDED_SOURCE_BACKED_ROUTES
}

/// Runtime metadata for one selected source route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRouteMetadata {
    pub source: ProviderSource,
    pub certified_source_format: &'static str,
    pub selection: Option<SourceBackedRouteSelection>,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteErrorKind {
    Unavailable,
    SourceChanged,
    InvalidSource,
    Unsupported,
    Internal,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{kind:?}: {detail}")]
pub struct SourceBackedRouteError {
    pub kind: SourceBackedRouteErrorKind,
    pub detail: String,
}

impl SourceBackedRouteError {
    pub fn new(kind: SourceBackedRouteErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SourceBackedCoordinatorError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("invalid source-backed route for {provider}: {detail}")]
    InvalidRoute {
        provider: CaptureProvider,
        detail: String,
    },
    #[error("source-backed scan failed for {provider}: {source}")]
    RouteScan {
        provider: CaptureProvider,
        #[source]
        source: SourceBackedRouteError,
    },
    #[error("source {source_id} was staged by more than one provider route")]
    DuplicateSourceOwner { source_id: String },
    #[error("no executable source-backed routes were registered")]
    NoExecutableRoutes,
    #[error("source-backed refresh progress callback failed: {0}")]
    Progress(SourceBackedRouteError),
}

/// The only write surface provider drivers receive. It exposes staging and
/// certification, but never generation commit.
pub struct SourceBackedGenerationSink<'writer> {
    writer: &'writer mut GenerationWriter,
    owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
    route_index: usize,
}

#[derive(Clone)]
struct SourceOwner {
    route_index: usize,
    source: SourceKey,
}

impl SourceBackedGenerationSink<'_> {
    pub fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.writer.base_manifest().and_then(|manifest| {
            manifest
                .sources
                .iter()
                .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
        })
    }

    pub fn begin_source(&mut self, source: SourceKey) -> SourceBackedCoordinatorResult<()> {
        self.claim(&source)?;
        self.writer.begin_source(source)?;
        Ok(())
    }

    pub fn begin_source_append(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource> {
        self.claim(&source)?;
        Ok(self.writer.begin_source_append(source)?)
    }

    pub fn add_document(&mut self, document: LexicalDocument) -> SourceBackedCoordinatorResult<()> {
        self.writer.add_document(document)?;
        Ok(())
    }

    pub fn certify_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_source(certificate)?;
        Ok(())
    }

    pub fn certify_source_append(
        &mut self,
        append: CertifiedSourceAppend,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_source_append(append)?;
        Ok(())
    }

    pub fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim(deletion.source())?;
        self.writer.delete_source(deletion)?;
        Ok(())
    }

    pub fn replace_source(
        &mut self,
        certificate: CertifiedSource,
        documents: impl IntoIterator<Item = LexicalDocument>,
    ) -> SourceBackedCoordinatorResult<()> {
        self.begin_source(certificate.observation().source().clone())?;
        for document in documents {
            self.add_document(document)?;
        }
        self.certify_source(certificate)
    }

    fn claim(&mut self, source: &SourceKey) -> SourceBackedCoordinatorResult<()> {
        let digest = source.identity().digest();
        match self.owners.get(&digest) {
            Some(owner)
                if owner.route_index != self.route_index
                    || !owner.source.exact_descriptor_eq(source) =>
            {
                return Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                    source_id: source.identity().to_string(),
                });
            }
            Some(_) => {}
            None => {
                self.owners.insert(
                    digest,
                    SourceOwner {
                        route_index: self.route_index,
                        source: source.clone(),
                    },
                );
            }
        }
        Ok(())
    }
}

pub enum SourceBackedRevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

trait ProviderCaptureSink {
    fn begin(&mut self, source: SourceKey) -> SourceBackedRouteResult<()>;
    fn document(&mut self, document: LexicalDocument) -> SourceBackedRouteResult<()>;
    fn certify(&mut self, certificate: CertifiedSource) -> SourceBackedRouteResult<()>;
}

struct WriterCaptureSink<'sink, 'writer> {
    sink: &'sink mut SourceBackedGenerationSink<'writer>,
}

impl ProviderCaptureSink for WriterCaptureSink<'_, '_> {
    fn begin(&mut self, source: SourceKey) -> SourceBackedRouteResult<()> {
        self.sink
            .begin_source(source)
            .map_err(route_coordinator_error)
    }

    fn document(&mut self, document: LexicalDocument) -> SourceBackedRouteResult<()> {
        self.sink
            .add_document(document)
            .map_err(route_coordinator_error)
    }

    fn certify(&mut self, certificate: CertifiedSource) -> SourceBackedRouteResult<()> {
        self.sink
            .certify_source(certificate)
            .map_err(route_coordinator_error)
    }
}

#[derive(Default)]
struct EvidenceCaptureSink {
    active: Option<SourceKey>,
    certificates: Vec<CertifiedSource>,
}

impl ProviderCaptureSink for EvidenceCaptureSink {
    fn begin(&mut self, source: SourceKey) -> SourceBackedRouteResult<()> {
        if self.active.replace(source).is_some() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "provider capture began a second source before certification",
            ));
        }
        Ok(())
    }

    fn document(&mut self, _document: LexicalDocument) -> SourceBackedRouteResult<()> {
        if self.active.is_none() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "provider capture emitted a document before beginning its source",
            ));
        }
        Ok(())
    }

    fn certify(&mut self, certificate: CertifiedSource) -> SourceBackedRouteResult<()> {
        let source = self.active.take().ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "provider capture certified a source that was not active",
            )
        })?;
        if !source.exact_descriptor_eq(certificate.observation().source()) {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "provider capture changed source descriptor before certification",
            ));
        }
        self.certificates.push(certificate);
        Ok(())
    }
}

type ProviderCaptureCallback =
    dyn Fn(&mut dyn ProviderCaptureSink) -> SourceBackedRouteResult<()> + Send + Sync;

fn captured_route_driver(
    capture: impl Fn(&mut dyn ProviderCaptureSink) -> SourceBackedRouteResult<()>
        + Send
        + Sync
        + 'static,
    owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
    hydrate: impl Fn(&EventHydrationRequest) -> Result<HydratedProviderRecord, HydrationFailure>
        + Send
        + Sync
        + 'static,
) -> SourceBackedRouteDriver {
    let capture: Arc<ProviderCaptureCallback> = Arc::new(capture);
    let scan_capture = Arc::clone(&capture);
    let revalidation_capture = Arc::clone(&capture);
    SourceBackedRouteDriver::new(
        move |sink| {
            let mut bridge = WriterCaptureSink { sink };
            scan_capture(&mut bridge)
        },
        owns_source,
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let mut evidence = EvidenceCaptureSink::default();
                revalidation_capture(&mut evidence).is_ok()
                    && evidence.active.is_none()
                    && evidence.certificates.iter().any(|candidate| {
                        candidate
                            .observation()
                            .source()
                            .exact_descriptor_eq(expected.observation().source())
                            && candidate == expected
                    })
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        hydrate,
    )
}

type ScanCallback = dyn for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
    + Send
    + Sync;
type SourcePredicate = dyn Fn(&SourceKey) -> bool + Send + Sync;
type RevalidationCallback =
    dyn for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync;
type HydrationCallback = dyn Fn(&EventHydrationRequest) -> Result<HydratedProviderRecord, HydrationFailure>
    + Send
    + Sync;

/// Closure bundle at the coordinator boundary. This deliberately does not
/// pretend provider scanners share a provider-local trait.
#[derive(Clone)]
pub struct SourceBackedRouteDriver {
    scan: Arc<ScanCallback>,
    owns_source: Arc<SourcePredicate>,
    revalidate: Arc<RevalidationCallback>,
    hydrate: Arc<HydrationCallback>,
}

impl fmt::Debug for SourceBackedRouteDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceBackedRouteDriver")
    }
}

impl SourceBackedRouteDriver {
    pub fn new(
        scan: impl for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
            + Send
            + Sync
            + 'static,
        owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
        revalidate: impl for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync + 'static,
        hydrate: impl Fn(&EventHydrationRequest) -> Result<HydratedProviderRecord, HydrationFailure>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            scan: Arc::new(scan),
            owns_source: Arc::new(owns_source),
            revalidate: Arc::new(revalidate),
            hydrate: Arc::new(hydrate),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceBackedRoute {
    metadata: SourceBackedRouteMetadata,
    driver: Option<SourceBackedRouteDriver>,
}

impl SourceBackedRoute {
    pub fn automatic(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
            },
            driver: Some(driver),
        })
    }

    pub fn explicit_manual(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
            },
            driver: Some(driver),
        })
    }

    pub fn unsupported(source: ProviderSource, reason: impl Into<String>) -> Self {
        let certified_source_format = landed_format_route(source.provider, source.source_format)
            .map_or(source.source_format, |route| route.certified_source_format);
        Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: None,
                selector_authority: SourceBackedSelectorAuthority::ExplicitPath,
                unsupported_reason: Some(reason.into()),
            },
            driver: None,
        }
    }

    pub fn metadata(&self) -> &SourceBackedRouteMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceBackedProviderRegistry {
    routes: Vec<SourceBackedRoute>,
}

impl SourceBackedProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, route: SourceBackedRoute) {
        self.routes.push(route);
    }

    pub fn routes(&self) -> impl ExactSizeIterator<Item = &SourceBackedRouteMetadata> {
        self.routes.iter().map(SourceBackedRoute::metadata)
    }

    pub fn resolver_registry(&self) -> SourceBackedResolverRegistry {
        SourceBackedResolverRegistry {
            routes: self.routes.clone(),
        }
    }

    pub fn executable_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_some())
            .count()
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_none())
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceBackedAutomaticUnavailableReason {
    SourceStatus(ProviderSourceStatus),
    UnsupportedFormat { detail: &'static str },
    SelectorAuthorityUnavailable { detail: &'static str },
    RegistrationRejected { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceBackedAutomaticRegistryIssue {
    Discovery(DiscoveryIssue),
    Unavailable {
        source: ProviderSource,
        reason: SourceBackedAutomaticUnavailableReason,
    },
}

#[derive(Debug, Clone)]
pub struct SourceBackedAutomaticRegistryBuild {
    pub registry: SourceBackedProviderRegistry,
    pub issues: Vec<SourceBackedAutomaticRegistryIssue>,
}

impl SourceBackedAutomaticRegistryBuild {
    pub fn executable_route_count(&self) -> usize {
        self.registry.executable_route_count()
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.registry.unsupported_route_count()
    }

    pub fn into_parts(
        self,
    ) -> (
        SourceBackedProviderRegistry,
        Vec<SourceBackedAutomaticRegistryIssue>,
    ) {
        (self.registry, self.issues)
    }
}

/// Discovers and registers every automatic source-backed route capture can
/// construct without daemon-side provider branching.
///
/// Normal provider absence and selector/discovery limitations are returned as
/// typed issues. A detected format whose adapter seam is unavailable is also
/// retained as a typed unsupported route, so refresh and hydration cannot
/// silently claim it.
pub fn build_automatic_source_backed_registry(
    discovery: &DiscoveryContext,
) -> SourceBackedAutomaticRegistryBuild {
    let report = discover_provider_sources_with_context(discovery);
    build_automatic_source_backed_registry_from_report(discovery, report.sources, report.issues)
}

fn build_automatic_source_backed_registry_from_report(
    discovery: &DiscoveryContext,
    sources: Vec<ProviderSource>,
    discovery_issues: Vec<DiscoveryIssue>,
) -> SourceBackedAutomaticRegistryBuild {
    let mut registry = SourceBackedProviderRegistry::new();
    let mut issues = discovery_issues
        .into_iter()
        .map(SourceBackedAutomaticRegistryIssue::Discovery)
        .collect::<Vec<_>>();
    let mut compound_provider_registered = HashSet::new();

    for source in sources {
        if source.import_support == ProviderImportSupport::Explicit {
            continue;
        }
        if source.import_support == ProviderImportSupport::Unsupported
            || source.source_kind == ProviderSourceKind::DetectionOnly
            || source.status == ProviderSourceStatus::Unsupported
            || source.unsupported_reason.is_some()
        {
            let detail = source
                .unsupported_reason
                .unwrap_or("the detected provider format is not supported for automatic refresh");
            retain_unsupported_automatic_format(&mut registry, &mut issues, source, detail);
            continue;
        }
        if !matches!(
            source.status,
            ProviderSourceStatus::Available | ProviderSourceStatus::Empty
        ) {
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
                reason: SourceBackedAutomaticUnavailableReason::SourceStatus(source.status),
                source,
            });
            continue;
        }

        let Some(format_route) = landed_format_route(source.provider, source.source_format) else {
            retain_unsupported_automatic_format(
                &mut registry,
                &mut issues,
                source,
                "the discovered provider format has no landed source-backed route",
            );
            continue;
        };
        if !format_route.automatic {
            retain_unsupported_automatic_format(
                &mut registry,
                &mut issues,
                source,
                "the discovered provider format is not registered for automatic refresh",
            );
            continue;
        }
        if let Some(reason) = format_route.unsupported_reason {
            retain_unsupported_automatic_format(&mut registry, &mut issues, source, reason);
            continue;
        }

        let compound_provider = matches!(
            source.provider,
            CaptureProvider::AstrBot | CaptureProvider::Crush | CaptureProvider::Lingma
        );
        if compound_provider && compound_provider_registered.contains(&source.provider) {
            continue;
        }

        match register_discovered_automatic_route(&mut registry, discovery, source.clone()) {
            Ok(()) => {
                if compound_provider {
                    compound_provider_registered.insert(source.provider);
                }
            }
            Err(reason) => {
                if compound_provider {
                    compound_provider_registered.insert(source.provider);
                }
                registry.register(SourceBackedRoute::unsupported(
                    source.clone(),
                    automatic_unavailable_detail(&reason),
                ));
                issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
            }
        }
    }

    SourceBackedAutomaticRegistryBuild { registry, issues }
}

fn retain_unsupported_automatic_format(
    registry: &mut SourceBackedProviderRegistry,
    issues: &mut Vec<SourceBackedAutomaticRegistryIssue>,
    source: ProviderSource,
    detail: &'static str,
) {
    registry.register(SourceBackedRoute::unsupported(source.clone(), detail));
    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
        source,
        reason: SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail },
    });
}

fn automatic_unavailable_detail(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("provider source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
        SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
    }
}

fn register_discovered_automatic_route(
    registry: &mut SourceBackedProviderRegistry,
    discovery: &DiscoveryContext,
    source: ProviderSource,
) -> Result<(), SourceBackedAutomaticUnavailableReason> {
    const CRUSH_SELECTOR_GAP: &str = "Crush discovery exposes database paths but not the stable project keys and rereadable finite inventory required by CrushProjectInventorySourceV0";
    const LINGMA_SELECTOR_GAP: &str = "Lingma discovery exposes selected databases but not the installed-client authority and per-database catalog lineages required by LingmaSourceInventoryV0";
    const WARP_SELECTOR_GAP: &str = "Warp discovery exposes database paths but not the stable installed-surface key required by WarpSourceSelectionV0";

    let result = match source.provider {
        CaptureProvider::Warp => {
            return Err(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: WARP_SELECTOR_GAP,
                },
            );
        }
        CaptureProvider::Goose => {
            let platform_root = goose_platform_root(discovery, &source.path).ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Goose discovery selected a database without its exact platform root",
                },
            )?;
            register_goose_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                platform_root,
                Vec::new(),
            )
        }
        CaptureProvider::Crush => {
            return Err(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: CRUSH_SELECTOR_GAP,
                },
            );
        }
        CaptureProvider::Lingma => {
            return Err(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: LINGMA_SELECTOR_GAP,
                },
            );
        }
        CaptureProvider::AstrBot => register_astrbot_source_backed_route(
            registry,
            source,
            SourceBackedRouteSelection::Automatic,
            discovery.clone(),
        ),
        CaptureProvider::Shelley => {
            let exact_cwd = discovery.cwd().ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Shelley automatic registration requires the exact discovery CWD",
                },
            )?;
            register_shelley_source_backed_route(registry, source, exact_cwd)
        }
        _ => register_landed_source_backed_route(
            registry,
            source,
            SourceBackedRouteSelection::Automatic,
        ),
    };
    result.map_err(
        |error| SourceBackedAutomaticUnavailableReason::RegistrationRejected {
            detail: error.to_string(),
        },
    )
}

fn goose_platform_root(discovery: &DiscoveryContext, database: &Path) -> Option<PathBuf> {
    if let Some(root) = discovery
        .env("GOOSE_PATH_ROOT")
        .filter(|value| !value.is_empty())
    {
        let root = PathBuf::from(root);
        if root.is_absolute() && database == root.join("data/sessions/sessions.db") {
            return Some(root);
        }
    }
    let root = match discovery.platform() {
        DiscoveryPlatform::Linux | DiscoveryPlatform::MacOS => {
            match discovery.env("XDG_DATA_HOME") {
                Some(value) if !value.is_empty() && Path::new(value).is_absolute() => {
                    PathBuf::from(value).join("goose")
                }
                _ => discovery.home().join(".local/share/goose"),
            }
        }
        DiscoveryPlatform::Windows => discovery
            .platform_dirs()
            .data
            .as_ref()?
            .join("Block/goose/data"),
        DiscoveryPlatform::OtherUnix => {
            let value = discovery
                .env("XDG_DATA_HOME")
                .filter(|value| !value.is_empty() && Path::new(value).is_absolute())?;
            PathBuf::from(value).join("goose")
        }
    };
    (database == root.join("sessions/sessions.db")).then_some(root)
}

#[derive(Debug, Clone)]
pub struct SourceBackedResolverRegistry {
    routes: Vec<SourceBackedRoute>,
}

impl ContentSourceResolver for SourceBackedResolverRegistry {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let source = request.locator().source();
        let mut matches = self.routes.iter().filter(|route| {
            route.metadata.source.provider.as_str() == source.provider()
                && route.metadata.certified_source_format == source.source_format()
                && route
                    .driver
                    .as_ref()
                    .is_some_and(|driver| (driver.owns_source)(source))
        });
        let Some(route) = matches.next() else {
            let unsupported = self.routes.iter().any(|route| {
                route.metadata.source.provider.as_str() == source.provider()
                    && route.metadata.certified_source_format == source.source_format()
                    && route.driver.is_none()
            });
            return Err(hydration_failure(
                if unsupported {
                    HydrationFailureKind::UnsupportedParserRevision
                } else {
                    HydrationFailureKind::InvalidLocator
                },
                if unsupported {
                    "the detected provider source format has no exact hydration route"
                } else {
                    "no registered provider route owns the exact source descriptor"
                },
            ));
        };
        if matches.next().is_some() {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "more than one provider route claimed the exact source descriptor",
            ));
        }
        let driver = route.driver.as_ref().ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "the provider route has no exact hydration driver",
            )
        })?;
        (driver.hydrate)(request)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        request
            .events()
            .iter()
            .map(|event| self.hydrate_event(event))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRefreshProgress {
    pub phase: &'static str,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub current_source: Option<String>,
}

/// Capture-owned executor that can be installed behind the daemon's
/// provider-neutral `SourceBackedRefreshExecutor` callback seam.
#[derive(Debug, Clone)]
pub struct SourceBackedRefreshExecutor {
    registry: SourceBackedProviderRegistry,
    writer_options: WriterOptions,
}

impl SourceBackedRefreshExecutor {
    pub fn new(registry: SourceBackedProviderRegistry, writer_options: WriterOptions) -> Self {
        Self {
            registry,
            writer_options,
        }
    }

    pub fn registry(&self) -> &SourceBackedProviderRegistry {
        &self.registry
    }

    pub fn refresh(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_progress(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            report_progress,
        )
    }
}

#[derive(Debug)]
pub struct SourceBackedRefreshReceipt {
    pub commit: CommitReceipt,
    pub scanned_routes: usize,
    pub unsupported_routes: Vec<SourceBackedRouteMetadata>,
}

/// Runs every executable route against one writer and publishes one atomic
/// generation. This is the capture-owned executor seam for the daemon.
pub fn refresh_source_backed_generation(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_progress(index_root, registry, writer_options, |_| Ok(()))
}

pub fn refresh_source_backed_generation_with_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    mut report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let scanned_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some())
        .count();
    if scanned_routes == 0 {
        return Err(SourceBackedCoordinatorError::NoExecutableRoutes);
    }
    report_progress(SourceBackedRefreshProgress {
        phase: "discovering",
        completed_sources: 0,
        total_sources: scanned_routes,
        current_source: None,
    })
    .map_err(SourceBackedCoordinatorError::Progress)?;
    let unsupported_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_none())
        .map(|route| route.metadata.clone())
        .collect();

    let mut writer = GenerationWriter::open(index_root, writer_options)?;
    let mut owners = HashMap::new();
    let mut completed_routes = 0;
    for (route_index, route) in registry.routes.iter().enumerate() {
        let Some(driver) = &route.driver else {
            continue;
        };
        report_progress(SourceBackedRefreshProgress {
            phase: "refreshing",
            completed_sources: completed_routes,
            total_sources: scanned_routes,
            current_source: Some(route.metadata.source.path.display().to_string()),
        })
        .map_err(SourceBackedCoordinatorError::Progress)?;
        let mut sink = SourceBackedGenerationSink {
            writer: &mut writer,
            owners: &mut owners,
            route_index,
        };
        (driver.scan)(&mut sink).map_err(|source| SourceBackedCoordinatorError::RouteScan {
            provider: route.metadata.source.provider,
            source,
        })?;
        completed_routes += 1;
    }

    report_progress(SourceBackedRefreshProgress {
        phase: "verifying",
        completed_sources: completed_routes,
        total_sources: scanned_routes,
        current_source: None,
    })
    .map_err(SourceBackedCoordinatorError::Progress)?;
    let commit = writer.commit(|target| {
        let source = match target {
            RevalidationTarget::Source(source) => source.observation().source(),
            RevalidationTarget::Deletion(deletion) => deletion.source(),
        };
        let Some(owner) = owners.get(&source.identity().digest()) else {
            return false;
        };
        if !owner.source.exact_descriptor_eq(source) {
            return false;
        }
        let Some(driver) = registry.routes[owner.route_index].driver.as_ref() else {
            return false;
        };
        match target {
            RevalidationTarget::Source(source) => {
                (driver.revalidate)(SourceBackedRevalidationTarget::Source(source))
            }
            RevalidationTarget::Deletion(deletion) => {
                (driver.revalidate)(SourceBackedRevalidationTarget::Deletion(deletion))
            }
        }
    })?;
    Ok(SourceBackedRefreshReceipt {
        commit,
        scanned_routes,
        unsupported_routes,
    })
}

/// Registers the landed Gemini adapter without moving any provider parsing
/// logic into the coordinator.
pub fn register_gemini_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let scan_root = root.clone();
    let revalidation_root = root.clone();
    let hydration_root = root;
    let driver = SourceBackedRouteDriver::new(
        move |sink| scan_gemini_route(&scan_root, sink),
        |source| {
            source.provider() == CaptureProvider::Gemini.as_str()
                && source.source_format() == GEMINI_CLI_SOURCE_FORMAT
        },
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                revalidate_gemini_source(&revalidation_root, expected)
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| hydrate_gemini_route(&hydration_root, request),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

/// Registers Cursor's sink-based adapter. Documents and its
/// `CertifiedSource` terminal are staged directly in the shared generation.
pub fn register_cursor_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let scan_root = root.clone();
    let revalidation_root = root.clone();
    let hydration_root = root;
    let driver = SourceBackedRouteDriver::new(
        move |sink| scan_cursor_route(&scan_root, sink),
        |source| {
            source.provider() == CaptureProvider::Cursor.as_str()
                && source.source_format() == CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
        },
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                revalidate_cursor_source(&revalidation_root, expected)
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| hydrate_cursor_route(&hydration_root, request),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

/// Mechanical registration entry for landed routes that require no additional
/// selector token beyond their selected path. Providers with compound
/// selectors have dedicated constructors so those selectors cannot be
/// fabricated here.
pub fn register_landed_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    match source.provider {
        CaptureProvider::Codex => register_codex_route(registry, source, selection),
        CaptureProvider::Zed => register_zed_route(registry, source, selection),
        CaptureProvider::Gemini => register_gemini_source_backed_route(registry, source, selection),
        CaptureProvider::Cursor => register_cursor_source_backed_route(registry, source, selection),
        CaptureProvider::Antigravity
        | CaptureProvider::Tabnine
        | CaptureProvider::Windsurf
        | CaptureProvider::CopilotCli
        | CaptureProvider::FactoryAiDroid
        | CaptureProvider::QwenCode
        | CaptureProvider::Qoder => register_direct_jsonl_route(registry, source, selection),
        CaptureProvider::CodeBuddy => register_codebuddy_route(registry, source, selection),
        CaptureProvider::Claude => register_claude_route(registry, source, selection),
        CaptureProvider::KiroCli => register_kiro_route(registry, source, selection),
        CaptureProvider::Auggie => register_auggie_route(registry, source, selection),
        CaptureProvider::Pi => register_pi_route(registry, source, selection),
        CaptureProvider::Junie => register_junie_route(registry, source, selection),
        CaptureProvider::KimiCodeCli => register_kimi_route(registry, source, selection),
        CaptureProvider::Firebender => register_firebender_route(registry, source, selection),
        CaptureProvider::DeepAgents => register_deepagents_route(registry, source, selection),
        CaptureProvider::ForgeCode => {
            register_forgecode_selected_route(registry, source, selection)
        }
        CaptureProvider::MistralVibe => register_mistral_route(registry, source, selection),
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            register_opencode_family_route(registry, source, selection)
        }
        CaptureProvider::OpenHands => register_openhands_route(registry, source, selection),
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            register_task_json_route(registry, source, selection)
        }
        CaptureProvider::Hermes => register_hermes_route(registry, source, selection),
        CaptureProvider::RovoDev => register_rovodev_route(registry, source, selection),
        CaptureProvider::Trae => register_trae_route(registry, source, selection),
        CaptureProvider::OpenClaw => register_openclaw_route(registry, source, selection),
        CaptureProvider::Continue => register_continue_route(registry, source, selection),
        CaptureProvider::Mux => register_mux_route(registry, source, selection),
        provider => Err(invalid_route(
            provider,
            "this provider requires its compound-selector registration constructor",
        )),
    }
}

fn register_codex_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let revalidation_root = root.clone();
    let hydration_root = root;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening = discover_codex_root_inventory_v0(&capture_root).map_err(route_error)?;
            let base_sources = codex_writer_base_sources(sink.writer);
            for (_, source_key, _) in &opening.sources {
                sink.claim(source_key).map_err(route_coordinator_error)?;
            }
            let mut revalidation = HashMap::new();
            let mut timings = CodexSourceBackedPhaseTimingsV0::default();
            let mut counters = CodexSourceBackedCountersV0::default();
            ingest_codex_sources_serial_v0(
                opening.sources.clone(),
                &base_sources,
                sink.writer,
                &mut revalidation,
                &mut timings,
                &mut counters,
            )
            .map_err(route_error)?;
            for base in base_sources.values() {
                let base_source = base.observation().source();
                if managed_codex_session_source(base_source)
                    && !opening.certificate.contains(base_source)
                {
                    sink.delete_source(
                        CertifiedSourceDeletion::from_inventory(
                            base_source.clone(),
                            &opening.certificate,
                        )
                        .map_err(route_error)?,
                    )
                    .map_err(route_coordinator_error)?;
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Codex, "codex_session_jsonl"),
        move |target| {
            let Ok(inventory) = discover_codex_root_inventory_v0(&revalidation_root) else {
                return false;
            };
            match target {
                SourceBackedRevalidationTarget::Source(expected) => inventory
                    .sources
                    .iter()
                    .find(|(_, source_key, _)| {
                        source_key.exact_descriptor_eq(expected.observation().source())
                    })
                    .and_then(|(source, source_key, _)| {
                        codex_source_observation(source_key, &source.catalog_observation).ok()
                    })
                    .is_some_and(|observation| observation == *expected.observation()),
                SourceBackedRevalidationTarget::Deletion(deletion) => {
                    deletion.verifies(&inventory.certificate)
                }
            }
        },
        move |request| {
            let hydrated = CodexLocatorResolverV0::discover([&hydration_root])
                .and_then(|resolver| resolver.hydrate(request.locator()))
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_zed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let revalidation_path = path.clone();
    let hydration_path = path;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let source_key = zed_source_key().map_err(route_error)?;
            let mut snapshot = acquire_zed_snapshot(&capture_path).map_err(route_error)?;
            let revision_digest = zed_snapshot_revision_digest(&snapshot.snapshot_revision);
            sink.begin_source(source_key.clone())
                .map_err(route_coordinator_error)?;
            let connection = snapshot.connection().map_err(route_error)?;
            let mut zed_sink = ZedSourceBackedSinkV0::new(
                sink.writer,
                connection,
                source_key.clone(),
                revision_digest,
                capture_path.to_string_lossy().into_owned(),
            )
            .map_err(route_error)?;
            let scan = scan_zed_native_snapshot(
                connection,
                &snapshot.physical_locator,
                &snapshot.snapshot_revision,
                &mut zed_sink,
            )
            .map_err(route_error)?;
            if let Some(error) = zed_sink.take_failure() {
                return Err(route_error(error));
            }
            let staged_documents = zed_sink.staged_documents();
            drop(zed_sink);
            snapshot.finish().map_err(route_error)?;
            if staged_documents != scan.counters.retained_events {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Zed source-backed counts do not reconcile",
                ));
            }
            let complete_records = scan
                .counters
                .retained_events
                .checked_add(scan.counters.rejected_threads)
                .ok_or_else(|| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Zed source-backed counts overflowed",
                    )
                })?;
            let counts = ScannedSourceCounts {
                complete_records,
                retained_records: scan.counters.retained_events,
                rejected_records: scan.counters.rejected_threads,
                ignored_records: 0,
                indexed_documents: staged_documents,
                certified_bytes: scan.counters.certified_logical_bytes,
            };
            let observation = zed_source_observation(&source_key, &snapshot.snapshot_revision)
                .map_err(route_error)?;
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                "zed-nativepath-source-backed-v0",
                decode_zed_digest(&scan.source_integrity_digest).map_err(route_error)?,
                counts,
            )
            .map_err(route_error)?;
            sink.certify_source(certificate)
                .map_err(route_coordinator_error)
        },
        provider_format_scope(CaptureProvider::Zed, "zed_threads_sqlite"),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(source_key) = zed_source_key() else {
                    return false;
                };
                let Ok(mut snapshot) = acquire_zed_snapshot(&revalidation_path) else {
                    return false;
                };
                let matches =
                    zed_source_observation(&source_key, &snapshot.snapshot_revision)
                        .is_ok_and(|observation| observation == *expected.observation());
                matches && snapshot.finish().is_ok()
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| {
            ZedLocatorResolverV0::new(&hydration_path)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_codebuddy_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            for scan in
                scan_codebuddy_source_backed_root(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                    .map_err(route_capture_error)?
            {
                sink.begin(scan.source.observation().source().clone())?;
                for page in scan.pages {
                    for document in page.documents {
                        sink.document(document)?;
                    }
                }
                sink.certify(scan.source)?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::CodeBuddy, "codebuddy_history_json"),
        move |request| {
            let hydrated =
                hydrate_codebuddy_source_backed_record(&hydration_root, request.locator())
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                    })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_claude_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let inventory = discover_claude_source_backed(&capture_root).map_err(route_error)?;
            for leaf in inventory.leaves() {
                let mut scanner =
                    ClaudeSourceBackedScanner::new(leaf.clone(), None).map_err(route_error)?;
                sink.begin(leaf.source_key().clone())?;
                while let Some(page) = scanner.next_page().map_err(route_error)? {
                    for document in page.documents {
                        sink.document(document)?;
                    }
                }
                let scan = scanner.finish().map_err(route_error)?;
                sink.certify(scan.source)?;
            }
            inventory.certify().map_err(route_error)?;
            Ok(())
        },
        provider_format_scope(CaptureProvider::Claude, "claude_projects_jsonl_tree"),
        move |request| {
            let hydrated = hydrate_claude_source_record(&hydration_root, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_kiro_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let source_format = source.source_format;
    let capture_path = path.clone();
    let hydration_path = path;
    let driver = captured_route_driver(
        move |sink| {
            let scan =
                scan_kiro_source_backed_v0(&capture_path, source_format).map_err(route_error)?;
            sink.begin(scan.source)?;
            for document in scan.documents {
                sink.document(document)?;
            }
            sink.certify(scan.certificate)
        },
        provider_format_scope(CaptureProvider::KiroCli, source_format),
        move |request| {
            let resolver = KiroLocatorResolverV0::discover(&hydration_path, source_format)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?;
            let hydrated = resolver.hydrate(request.locator()).map_err(|error| {
                hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
            })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_auggie_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = AuggieSourceBackedRoot::explicit(source.path.clone());
    let capture_root = root.clone();
    let context = ProviderAdapterContext {
        machine_id: "source-backed-auggie".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let capture_context = context.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let inventory = discover_auggie_source_backed(&capture_root).map_err(route_error)?;
            for projected in project_auggie_source_backed_inventory(&inventory, &capture_context)
                .map_err(route_error)?
            {
                sink.begin(projected.certified_source.observation().source().clone())?;
                for document in projected.documents {
                    sink.document(document)?;
                }
                sink.certify(projected.certified_source)?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Auggie, "auggie_session_json"),
        move |request| {
            let inventory = discover_auggie_source_backed(&hydration_root).map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            for path in inventory.paths {
                if let Ok(hydrated) = hydrate_auggie_source_backed(&path, request.locator()) {
                    return Ok(HydratedProviderRecord {
                        event_id: request.event_id(),
                        provider_bytes: hydrated.provider_bytes,
                    });
                }
            }
            Err(hydration_failure(
                HydrationFailureKind::MissingRecord,
                "the exact Auggie source record is absent",
            ))
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_pi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = match selection {
        SourceBackedRouteSelection::Automatic => {
            PiSourceBackedRoot::winning(source.path.clone())
                .map_err(|error| invalid_route(source.provider, error.to_string()))?
        }
        SourceBackedRouteSelection::ExplicitManual => {
            PiSourceBackedRoot::explicit(source.path.clone())
        }
    };
    let context = ProviderAdapterContext {
        machine_id: "source-backed-pi".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let capture_root = root.clone();
    let capture_context = context.clone();
    let hydration_root = root;
    let hydration_context = context;
    let driver = captured_route_driver(
        move |sink| {
            let mut begun = HashSet::new();
            let mut sink_failure = None;
            let projection = project_pi_source_backed_root_cold(
                &capture_root,
                capture_context.clone(),
                |page| {
                    if sink_failure.is_some() {
                        return;
                    }
                    if begun.insert(page.source.identity().digest()) {
                        if let Err(error) = sink.begin(page.source) {
                            sink_failure = Some(error);
                            return;
                        }
                    }
                    for document in page.documents {
                        if let Err(error) = sink.document(document) {
                            sink_failure = Some(error);
                            return;
                        }
                    }
                },
            )
            .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            for source in projection.sources {
                if begun.insert(source.route.source.identity().digest()) {
                    sink.begin(source.route.source)?;
                }
                sink.certify(source.certificate)?;
            }
            let _inventory = projection.inventory;
            Ok(())
        },
        provider_format_scope(CaptureProvider::Pi, "pi_session_jsonl"),
        move |request| {
            let projection = project_pi_source_backed_root_cold(
                &hydration_root,
                hydration_context.clone(),
                |_| {},
            )
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            PiSourceBackedResolver::new(projection.sources.into_iter().map(|source| source.route))
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_forgecode_selected_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(invalid_route(
            source.provider,
            "manual ForgeCode registration requires explicit catalog lineage",
        ));
    }
    register_forgecode_route(
        registry,
        source,
        selection,
        ForgeCodeSourceSelectionV0::selected,
    )
}

pub fn register_forgecode_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    register_forgecode_route(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        move |path| ForgeCodeSourceSelectionV0::explicit(path, catalog_lineage),
    )
}

/// Registers one caller-owned Custom History JSONL route. The path is only a
/// resolver location; `catalog_lineage` remains the durable source identity.
pub fn register_custom_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let input = CustomHistorySourceBackedInput::explicit(source.path.clone(), catalog_lineage);
    let owned_source = input
        .source_key()
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let scan_input = input.clone();
    let revalidation_input = input.clone();
    let hydration_input = input;
    let claimed_source = owned_source.clone();
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening =
                observe_custom_history_source_backed_explicit(&scan_input).map_err(route_error)?;
            let base = sink.base_source(&claimed_source).cloned();
            if opening.is_missing() {
                let outcome =
                    scan_custom_history_source_backed_explicit(&scan_input, base.as_ref(), |_| {
                        Ok(())
                    })
                    .map_err(route_error)?;
                let CustomHistorySourceBackedOutcome::Missing { deletion, .. } = outcome else {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::SourceChanged,
                        "Custom History source appeared after its opening observation",
                    ));
                };
                if let Some(deletion) = deletion {
                    sink.delete_source(deletion)
                        .map_err(route_coordinator_error)?;
                }
                return Ok(());
            }

            sink.begin_source(claimed_source.clone())
                .map_err(route_coordinator_error)?;
            let outcome = scan_custom_history_source_backed_explicit(&scan_input, None, |page| {
                for document in page.documents {
                    sink.add_document(document)
                        .map_err(capture_coordinator_error)?;
                }
                Ok(())
            })
            .map_err(route_error)?;
            let CustomHistorySourceBackedOutcome::Present(receipt) = outcome else {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Custom History source disappeared during its replacement scan",
                ));
            };
            if !matches!(
                receipt.disposition,
                CustomHistorySourceBackedDisposition::Cold
            ) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "cold Custom History scan returned a non-cold disposition",
                ));
            }
            sink.certify_source(receipt.certificate)
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(certificate) => {
                revalidate_custom_history_source_backed(&revalidation_input, certificate)
                    .unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                let Ok(opening) =
                    observe_custom_history_source_backed_explicit(&revalidation_input)
                else {
                    return false;
                };
                let Ok(closing) =
                    observe_custom_history_source_backed_explicit(&revalidation_input)
                else {
                    return false;
                };
                opening
                    .certify_against(&closing)
                    .is_ok_and(|inventory| deletion.verifies(&inventory))
            }
        },
        move |request| {
            let outcome =
                scan_custom_history_source_backed_explicit(&hydration_input, None, |_| Ok(()))
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?;
            let CustomHistorySourceBackedOutcome::Present(receipt) = outcome else {
                return Err(hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "the explicit Custom History source is absent",
                ));
            };
            CustomHistorySourceBackedResolver::new([receipt.route])
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
                .hydrate_event(request)
        },
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}

/// Registers one explicit NanoClaw compound project with caller-owned catalog
/// lineage.
pub fn register_nanoclaw_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let owned_source = nanoclaw_source_key(catalog_lineage)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let driver = captured_route_driver(
        move |sink| {
            sink.begin(owned_source.clone())?;
            let receipt =
                scan_nanoclaw_source_backed(&capture_path, catalog_lineage, |page| {
                    for document in page.documents {
                        sink.document(document).map_err(|error| {
                            super::providers::nanoclaw::native_path::source_backed::NanoClawSourceBackedError::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })?;
                    }
                    Ok(())
                })
                .map_err(route_error)?;
            sink.certify(receipt.source)
        },
        provider_format_scope(CaptureProvider::NanoClaw, "nanoclaw_project"),
        move |request| {
            let record = hydrate_nanoclaw_source_backed_exact(
                &hydration_path,
                catalog_lineage,
                request.locator(),
            )
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: record.text.into_bytes(),
            })
        },
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}

/// Registers a Warp database under its stable installed-surface key.
pub fn register_warp_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    surface_key: impl Into<String>,
) -> SourceBackedCoordinatorResult<()> {
    let selected = WarpSourceSelectionV0::new(source.path.clone(), surface_key)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let capture_selection = selected.clone();
    let hydration_selection = selected;
    let driver = captured_route_driver(
        move |sink| {
            let snapshot =
                project_warp_source_backed_v0(capture_selection.clone()).map_err(route_error)?;
            sink.begin(snapshot.source)?;
            for document in snapshot.documents {
                sink.document(document)?;
            }
            sink.certify(snapshot.certified_source)
        },
        provider_format_scope(CaptureProvider::Warp, "warp_sqlite"),
        move |request| {
            let hydrated = resolve_warp_locator_v0(&hydration_selection, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::NamedSurface,
        driver,
    )?);
    Ok(())
}

/// Registers Goose's selected database and the exact platform root needed to
/// resolve attachments. Historical routes are retained only when supplied.
pub fn register_goose_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    platform_root: impl Into<std::path::PathBuf>,
    retained_routes: Vec<(std::path::PathBuf, std::path::PathBuf)>,
) -> SourceBackedCoordinatorResult<()> {
    let mut selected =
        GooseSourceBackedSelectionV0::exact(source.path.clone(), platform_root.into());
    if !retained_routes.is_empty() {
        selected = selected
            .with_explicit_retained_routes(
                retained_routes
                    .into_iter()
                    .map(|(database, root)| GooseSourceRouteV0::exact(database, root))
                    .collect(),
            )
            .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    }
    let capture_selection = selected.clone();
    let hydration_selection = selected;
    let driver = captured_route_driver(
        move |sink| {
            let adapter =
                GooseSourceBackedAdapterV0::open(capture_selection.clone()).map_err(route_error)?;
            sink.begin(adapter.source().clone())?;
            let mut scan = adapter.scan().map_err(route_error)?;
            while let Some(page) = scan.next_page().map_err(route_error)? {
                for document in page.into_documents() {
                    sink.document(document)?;
                }
            }
            sink.certify(scan.finish().map_err(route_error)?.certificate().clone())
        },
        provider_format_scope(CaptureProvider::Goose, "goose_sessions_sqlite"),
        move |request| {
            GooseSourceBackedResolverV0::new(hydration_selection.clone())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        driver,
    )?);
    Ok(())
}

/// Registers the finite Lingma database inventory supplied by product
/// discovery. Database lineage and inventory authority are caller-owned.
pub fn register_lingma_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    authority_key: TypedKey,
    databases: Vec<(std::path::PathBuf, TypedKey)>,
) -> SourceBackedCoordinatorResult<()> {
    let databases = databases
        .into_iter()
        .map(|(path, lineage)| LingmaDatabaseSourceV0::new(path, lineage))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let inventory = LingmaSourceInventoryV0::new(authority_key, databases)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let capture_inventory = inventory.clone();
    let hydration_inventory = inventory;
    let driver = captured_route_driver(
        move |sink| {
            let closing = capture_inventory.clone();
            let scan = scan_lingma_source_backed_v0(capture_inventory.clone(), move || Ok(closing))
                .map_err(route_error)?;
            for database in scan.databases() {
                sink.begin(database.certificate().observation().source().clone())?;
                for record in database.records() {
                    sink.document(record.document().clone())?;
                }
                sink.certify(database.certificate().clone())?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Lingma, "lingma_sqlite"),
        move |request| {
            let result = LingmaSourceBackedResolverV0::new(&hydration_inventory)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate(request.event_id(), request.locator());
            match result {
                Ok(content) => Ok(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes: content.text.into_bytes(),
                }),
                Err(error) => Err(hydration_failure(
                    match error.kind {
                        LingmaExactContentFailureKindV0::ExactContentUnavailable => {
                            HydrationFailureKind::UnsupportedParserRevision
                        }
                        LingmaExactContentFailureKindV0::InvalidLocator => {
                            HydrationFailureKind::InvalidLocator
                        }
                        LingmaExactContentFailureKindV0::SourceUnavailable => {
                            HydrationFailureKind::TemporarilyUnavailable
                        }
                        LingmaExactContentFailureKindV0::RecordMissing => {
                            HydrationFailureKind::MissingRecord
                        }
                        LingmaExactContentFailureKindV0::StaleRecordEvidence => {
                            HydrationFailureKind::StaleRecordEvidence
                        }
                    },
                    error,
                )),
            }
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

/// Registers Crush's selector-owned finite project inventory. The coordinator
/// consumes the adapter's existing scan helpers but remains the only owner of
/// `GenerationWriter` and commit.
pub fn register_crush_source_backed_route<I>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    inventory_source: Arc<I>,
) -> SourceBackedCoordinatorResult<()>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
{
    let scan_inventory = Arc::clone(&inventory_source);
    let revalidation_inventory = Arc::clone(&inventory_source);
    let hydration_inventory = inventory_source;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening = bind_crush_inventory(scan_inventory.observe().map_err(route_error)?)
                .map_err(route_error)?;
            let base_sources = sink
                .writer
                .base_manifest()
                .map(|manifest| {
                    manifest
                        .sources
                        .iter()
                        .cloned()
                        .map(|certificate| {
                            (certificate.observation().source().clone(), certificate)
                        })
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();

            for database in &opening.databases {
                let opened = open_crush_source(database.clone()).map_err(route_error)?;
                let base = base_sources.get(&database.source_key);
                if base.is_some_and(|base| crush_exact_replay_matches(base, &opened)) {
                    if !finish_crush_source(opened).map_err(route_error)? {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush source changed while its replay was staged",
                        ));
                    }
                    let base = base.ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "Crush replay base disappeared",
                        )
                    })?;
                    let writer_base = sink
                        .begin_source_append(database.source_key.clone())
                        .map_err(route_coordinator_error)?;
                    if writer_base != base {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush replay base changed inside the shared writer",
                        ));
                    }
                    let frontier = base.frontier().ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::InvalidSource,
                            "Crush replay base has no exact frontier",
                        )
                    })?;
                    sink.certify_source_append(
                        CertifiedSourceAppend::certify(
                            base,
                            base.clone(),
                            frontier.certified_prefix_bytes(),
                            *frontier.certified_prefix_digest(),
                        )
                        .map_err(route_error)?,
                    )
                    .map_err(route_coordinator_error)?;
                } else {
                    sink.begin_source(database.source_key.clone())
                        .map_err(route_coordinator_error)?;
                    let scan = scan_crush_source(&opened, sink.writer).map_err(route_error)?;
                    let closing = closing_crush_observation(&opened).map_err(route_error)?;
                    let opening_observation = opened.observation.clone();
                    if !finish_crush_source(opened).map_err(route_error)? {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush source changed while its replacement was staged",
                        ));
                    }
                    let frontier = SourceFrontier::new(
                        CRUSH_FRONTIER_KIND,
                        TypedKey::bytes(opening_observation.revision().to_vec())
                            .map_err(route_error)?,
                        scan.counts.certified_bytes,
                        scan.content_digest,
                    )
                    .map_err(route_error)?;
                    let certificate = CertifiedSource::certify_with_frontier(
                        opening_observation,
                        closing,
                        CRUSH_PARSER_REVISION,
                        scan.content_digest,
                        scan.counts,
                        Some(frontier),
                    )
                    .map_err(route_error)?;
                    sink.certify_source(certificate)
                        .map_err(route_coordinator_error)?;
                }
            }

            let closing_observation = scan_inventory.observe().map_err(route_error)?;
            if !opening
                .matches(closing_observation.clone())
                .map_err(route_error)?
            {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Crush project inventory changed during shared staging",
                ));
            }
            let closing = bind_crush_inventory(closing_observation).map_err(route_error)?;
            let certified_inventory = CertifiedSourceInventory::certify(
                opening.observation.clone(),
                closing.observation,
                CRUSH_DISCOVERY_REVISION,
                opening.source_keys(),
            )
            .map_err(route_error)?;
            for base in base_sources.values() {
                let base_source = base.observation().source();
                if base_source.provider() == CaptureProvider::Crush.as_str()
                    && base_source.source_format() == "crush_sqlite"
                    && base_source.schema_variant() == CRUSH_SOURCE_SCHEMA_VARIANT
                    && !opening.contains_exact_source(base_source)
                {
                    sink.delete_source(
                        CertifiedSourceDeletion::from_inventory(
                            base_source.clone(),
                            &certified_inventory,
                        )
                        .map_err(route_error)?,
                    )
                    .map_err(route_coordinator_error)?;
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Crush, "crush_sqlite"),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(observation) = revalidation_inventory.observe() else {
                    return false;
                };
                let Ok(inventory) = bind_crush_inventory(observation) else {
                    return false;
                };
                let Some(database) = inventory.databases.iter().find(|database| {
                    database
                        .source_key
                        .exact_descriptor_eq(expected.observation().source())
                }) else {
                    return false;
                };
                let Ok(opened) = open_crush_source(database.clone()) else {
                    return false;
                };
                let observation_matches = opened.observation == *expected.observation();
                observation_matches && finish_crush_source(opened).unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                let Ok(opening_observation) = revalidation_inventory.observe() else {
                    return false;
                };
                let Ok(opening) = bind_crush_inventory(opening_observation.clone()) else {
                    return false;
                };
                let Ok(closing_observation) = revalidation_inventory.observe() else {
                    return false;
                };
                if !opening
                    .matches(closing_observation.clone())
                    .unwrap_or(false)
                {
                    return false;
                }
                let Ok(closing) = bind_crush_inventory(closing_observation) else {
                    return false;
                };
                let source_keys = opening.source_keys();
                CertifiedSourceInventory::certify(
                    opening.observation,
                    closing.observation,
                    CRUSH_DISCOVERY_REVISION,
                    source_keys,
                )
                .is_ok_and(|inventory| deletion.verifies(&inventory))
            }
        },
        move |request| {
            let hydrated = CrushLocatorResolverV0::discover(hydration_inventory.as_ref())
                .and_then(|resolver| resolver.hydrate(request.locator()))
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            let provider_bytes = hydrated
                .decoded_display_text
                .ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::UnsupportedParserRevision,
                        "Crush record has no exact display text",
                    )
                })?
                .into_bytes();
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        driver,
    )?);
    Ok(())
}

/// Registers AstrBot's complete selected/launcher inventory from the same
/// bounded discovery context used by provider selection.
pub fn register_astrbot_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    discovery: DiscoveryContext,
) -> SourceBackedCoordinatorResult<()> {
    let capture_discovery = discovery.clone();
    let hydration_discovery = discovery;
    let driver = captured_route_driver(
        move |sink| {
            let opening = AstrBotSourceBackedInventoryV0::discover(&capture_discovery)
                .map_err(route_error)?;
            for selected in opening.sources() {
                sink.begin(selected.source_key().clone())?;
                let certificate =
                    scan_astrbot_source_backed_v0(selected, &mut |document| {
                        sink.document(document).map_err(|error| {
                            super::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })
                    })
                    .map_err(route_error)?;
                sink.certify(certificate)?;
            }
            let closing = AstrBotSourceBackedInventoryV0::discover(&capture_discovery)
                .map_err(route_error)?;
            opening.certify(&closing).map_err(route_error)?;
            Ok(())
        },
        provider_format_scope(CaptureProvider::AstrBot, "astrbot_data_v4_sqlite"),
        move |request| {
            let inventory = AstrBotSourceBackedInventoryV0::discover(&hydration_discovery)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?;
            AstrBotSourceBackedResolverV0::from_inventory(&inventory)
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

/// Registers Shelley only when the caller retains the exact CWD that selected
/// `shelley.db`. No branch or fallback CWD is inferred.
pub fn register_shelley_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    exact_cwd: impl Into<std::path::PathBuf>,
) -> SourceBackedCoordinatorResult<()> {
    let exact_cwd = exact_cwd.into();
    let adapter = discover_shelley_source_backed_exact_cwd(&exact_cwd)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?
        .ok_or_else(|| {
            invalid_route(
                source.provider,
                "the exact Shelley CWD no longer contains an admitted database",
            )
        })?;
    if adapter.database_path() != source.path {
        return Err(invalid_route(
            source.provider,
            "the Shelley source path does not belong to the supplied exact CWD",
        ));
    }
    register_shelley_adapter(registry, source, adapter)
}

fn register_shelley_adapter(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    adapter: ShelleySourceBackedAdapter,
) -> SourceBackedCoordinatorResult<()> {
    let capture_adapter = adapter.clone();
    let hydration_adapter = adapter;
    let driver = captured_route_driver(
        move |sink| {
            sink.begin(capture_adapter.source().clone())?;
            let mut scan = capture_adapter.start_scan().map_err(route_error)?;
            while let Some(page) = scan.next_page().map_err(route_error)? {
                for document in page.documents {
                    sink.document(document)?;
                }
            }
            sink.certify(scan.finish().map_err(route_error)?.certificate)
        },
        provider_format_scope(CaptureProvider::Shelley, "shelley_sqlite"),
        move |request| {
            let hydrated = hydration_adapter
                .hydrate(request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.text.into_bytes(),
            })
        },
    );
    registry.register(SourceBackedRoute::automatic(
        source,
        SourceBackedSelectorAuthority::ExactCwd,
        driver,
    )?);
    Ok(())
}

/// Registers an inactive Hermes database only with a caller-owned persistent
/// anchor. Automatic profile routes continue to use provider-native profile
/// identity.
pub fn register_hermes_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    anchor: SourceAnchor,
) -> SourceBackedCoordinatorResult<()> {
    let candidate = hermes_source_backed_explicit(source.path.clone(), anchor)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_hermes_candidate(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        candidate,
        SourceBackedSelectorAuthority::ExplicitPath,
    )
}

fn register_forgecode_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    make_selection: impl Fn(std::path::PathBuf) -> ForgeCodeSourceSelectionV0
        + Send
        + Sync
        + Clone
        + 'static,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let capture_selection = make_selection.clone();
    let hydration_selection = make_selection;
    let driver = captured_route_driver(
        move |sink| {
            let ForgeCodeSourceBackedDiscoveryV0::Live(mut scan) =
                open_forgecode_source_backed_v0(capture_selection(capture_path.clone()))
                    .map_err(route_error)?
            else {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "selected ForgeCode database is missing",
                ));
            };
            let route = scan.source().clone();
            sink.begin(route.source().clone())?;
            while let Some(page) = scan.next_page().map_err(route_error)? {
                for document in page.documents {
                    sink.document(document)?;
                }
            }
            sink.certify(scan.finish().map_err(route_error)?)
        },
        provider_format_scope(CaptureProvider::ForgeCode, "forgecode_sqlite"),
        move |request| {
            let ForgeCodeSourceBackedDiscoveryV0::Live(scan) =
                open_forgecode_source_backed_v0(hydration_selection(hydration_path.clone()))
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?
            else {
                return Err(hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "selected ForgeCode database is missing",
                ));
            };
            ForgeCodeSourceBackedResolverV0::new([scan.source().clone()])
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        if selection == SourceBackedRouteSelection::Automatic {
            SourceBackedSelectorAuthority::SelectedWithRetainedExplicit
        } else {
            SourceBackedSelectorAuthority::ExplicitPath
        },
        driver,
    )?);
    Ok(())
}

fn register_junie_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let mut scanner =
                JunieSourceBackedScannerV0::discover(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                    .map_err(route_error)?;
            while let Some(emission) = scanner.next_page().map_err(route_error)? {
                match emission {
                    JunieSourceBackedEmissionV0::BeginSource(source) => sink.begin(source)?,
                    JunieSourceBackedEmissionV0::Documents(documents) => {
                        for document in documents {
                            sink.document(document)?;
                        }
                    }
                    JunieSourceBackedEmissionV0::CertifiedSource(certificate) => {
                        sink.certify(certificate)?;
                    }
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Junie, "junie_session_events_jsonl_tree"),
        move |request| {
            let resolver = JunieLocatorResolverV0::discover(&hydration_root).map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            resolver.hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_kimi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let catalog = KimiSourceBackedCatalog::discover(&capture_root).map_err(route_error)?;
            let sources = catalog.source_keys().cloned().collect::<Vec<_>>();
            for source in sources {
                sink.begin(source.clone())?;
                let certificate = catalog
                    .scan_source(&source, |document| {
                        sink.document(document).map_err(|error| {
                            super::providers::kimi::native_path::source_backed::KimiSourceBackedError::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })
                    })
                    .map_err(route_error)?;
                sink.certify(certificate)?;
            }
            if !catalog.revalidate_inventory().map_err(route_error)? {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Kimi catalog changed before shared publication",
                ));
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::KimiCodeCli, "kimi_code_cli_wire_jsonl"),
        move |request| {
            let catalog = KimiSourceBackedCatalog::discover(&hydration_root).map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            KimiSourceBackedResolver::new(catalog).hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_firebender_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let driver = captured_route_driver(
        move |sink| {
            let FirebenderSourceBackedPlan::Replacement(mut scanner) =
                prepare_firebender_source_backed(&capture_path, None).map_err(route_error)?
            else {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "cold Firebender scan unexpectedly returned an unchanged certificate",
                ));
            };
            sink.begin(scanner.source().clone())?;
            while let Some(page) = scanner.next_page().map_err(route_error)? {
                for document in page.into_documents() {
                    sink.document(document)?;
                }
            }
            sink.certify(scanner.finish().map_err(route_error)?)
        },
        provider_format_scope(
            CaptureProvider::Firebender,
            "firebender_chat_history_sqlite",
        ),
        move |request| {
            let hydrated = hydrate_firebender_source_backed_row(&hydration_path, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.messages_json().to_vec(),
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_deepagents_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let database = source.path.clone();
    let capture_database = database.clone();
    let hydration_database = database;
    let driver = captured_route_driver(
        move |sink| {
            let mut scanner = DeepAgentsSourceBackedScannerV0::open(
                DeepAgentsDatabaseSelectionV0::explicit(capture_database.clone()),
                DateTime::<Utc>::UNIX_EPOCH,
            )
            .map_err(route_error)?;
            sink.begin(scanner.source().clone())?;
            while let Some(page) = scanner.next_page().map_err(route_error)? {
                for document in page {
                    sink.document(document)?;
                }
            }
            sink.certify(scanner.finish().map_err(route_error)?.certificate)
        },
        provider_format_scope(CaptureProvider::DeepAgents, "deepagents_sessions_sqlite"),
        move |request| {
            let hydrated = DeepAgentsLocatorResolverV0::explicit(hydration_database.clone())
                .hydrate(request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.text.into_bytes(),
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_mistral_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let scan = scan_mistral_vibe_source_backed(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                .map_err(route_error)?;
            for leaf in scan.leaves {
                sink.begin(leaf.source.observation().source().clone())?;
                for document in leaf.documents {
                    sink.document(document)?;
                }
                sink.certify(leaf.source)?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::MistralVibe, "mistral_vibe_session_jsonl"),
        move |request| {
            let scan =
                scan_mistral_vibe_source_backed(&hydration_root, DateTime::<Utc>::UNIX_EPOCH)
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?;
            scan.resolver.hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_opencode_family_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let registration = match source.provider {
        CaptureProvider::OpenCode => opencode_source_backed_registration(),
        CaptureProvider::Kilo => kilo_source_backed_registration(),
        CaptureProvider::MiMoCode => mimocode_source_backed_registration(),
        _ => unreachable!("caller restricts the OpenCode family"),
    };
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let provider = source.provider;
    let source_format = source.source_format;
    let driver = captured_route_driver(
        move |sink| {
            let mut began = false;
            let mut sink_failure = None;
            let scan = registration
                .scan(&capture_path, &mut |page| {
                    for document in page {
                        if !began {
                            if let Err(error) = sink.begin(document.source.clone()) {
                                let detail = error.to_string();
                                sink_failure = Some(error);
                                return Err(OpenCodeSourceBackedError::Capture(
                                    CaptureError::InvalidPayload(detail),
                                ));
                            }
                            began = true;
                        }
                        if let Err(error) = sink.document(document) {
                            let detail = error.to_string();
                            sink_failure = Some(error);
                            return Err(OpenCodeSourceBackedError::Capture(
                                CaptureError::InvalidPayload(detail),
                            ));
                        }
                    }
                    Ok(())
                })
                .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            if !began {
                sink.begin(scan.source)?;
            }
            sink.certify(scan.certificate)
        },
        provider_format_scope(provider, source_format),
        move |request| {
            registration
                .exact_resolver(hydration_path.clone())
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_openhands_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let adapter =
                OpenHandsSourceBackedAdapterV1::discover(&capture_root).map_err(route_error)?;
            let projection = adapter.project().map_err(route_error)?;
            for certificate in projection.sources() {
                sink.begin(certificate.observation().source().clone())?;
                for document in projection.documents().iter().filter(|document| {
                    document
                        .source
                        .exact_descriptor_eq(certificate.observation().source())
                }) {
                    sink.document(document.clone())?;
                }
                sink.certify(certificate.clone())?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::OpenHands, "openhands_file_events"),
        move |request| {
            let resolver =
                OpenHandsLocatorResolverV1::discover(&hydration_root).map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?;
            resolver.hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_task_json_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let selected = vec![source.clone()];
    let capture_selected = selected.clone();
    let hydration_selected = selected;
    let provider = source.provider;
    let source_format = source.source_format;
    let driver = captured_route_driver(
        move |sink| {
            let mut adapter = match provider {
                CaptureProvider::Cline => cline_task_json_source_backed_adapter(&capture_selected),
                CaptureProvider::RooCode => roo_task_json_source_backed_adapter(&capture_selected),
                _ => unreachable!("caller restricts task JSON providers"),
            };
            if !adapter.detected_but_unsupported().is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unsupported,
                    "the selected task directory is a detected but unsupported format",
                ));
            }
            if !adapter.unavailable().is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "the selected task directory is unavailable",
                ));
            }
            let mut begun = HashSet::new();
            while let Some(page) = adapter.next_page().map_err(route_error)? {
                let digest = page.source.identity().digest();
                if begun.insert(digest) {
                    sink.begin(page.source)?;
                }
                for document in page.documents {
                    sink.document(document)?;
                }
            }
            let completion = adapter.finish().map_err(route_error)?;
            if !completion.detected_but_unsupported.is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unsupported,
                    "task discovery completed with an unsupported detected format",
                ));
            }
            if !completion.unavailable.is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "task discovery completed with an unavailable selected route",
                ));
            }
            for task in completion.tasks {
                if begun.insert(task.source.identity().digest()) {
                    sink.begin(task.source)?;
                }
                sink.certify(task.certified_source)?;
            }
            let _certified_inventories = completion.inventories.len();
            Ok(())
        },
        provider_format_scope(provider, source_format),
        move |request| {
            let resolver = match provider {
                CaptureProvider::Cline => {
                    cline_task_json_source_backed_resolver(&hydration_selected)
                }
                CaptureProvider::RooCode => {
                    roo_task_json_source_backed_resolver(&hydration_selected)
                }
                _ => unreachable!("caller restricts task JSON providers"),
            }
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            resolver.hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_hermes_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(invalid_route(
            source.provider,
            "manual Hermes registration requires a persistent explicit SourceAnchor",
        ));
    }
    let candidate = HermesSourceCandidate::automatic(source.clone())
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_hermes_candidate(
        registry,
        source,
        selection,
        candidate,
        SourceBackedSelectorAuthority::DiscoveredWinner,
    )
}

fn register_hermes_candidate(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    candidate: HermesSourceCandidate,
    authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<()> {
    let capture_candidate = candidate.clone();
    let hydration_path = candidate.path().to_path_buf();
    let driver = captured_route_driver(
        move |sink| {
            sink.begin(capture_candidate.source().clone())?;
            let mut sink_failure = None;
            let certificate = scan_hermes_source_backed(&capture_candidate, |page| {
                for record in page.records {
                    if let HermesSourceBackedRecord::Event(document) = record {
                        if let Err(error) = sink.document(document) {
                            let detail = error.to_string();
                            sink_failure = Some(error);
                            return Err(HermesSourceBackedError::Capture(
                                CaptureError::InvalidPayload(detail),
                            ));
                        }
                    }
                }
                Ok(())
            })
            .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            sink.certify(certificate)
        },
        provider_format_scope(CaptureProvider::Hermes, "hermes_state_sqlite"),
        move |request| {
            let hydrated = hydrate_hermes_source_backed_message(&hydration_path, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(source, selection, authority, driver)?);
    Ok(())
}

fn register_rovodev_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let context = ProviderAdapterContext {
        machine_id: "source-backed-rovodev".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let capture_root = root.clone();
    let capture_context = context.clone();
    let hydration_root = root;
    let hydration_context = context;
    let driver = captured_route_driver(
        move |sink| {
            let inventory = discover_rovodev_source_backed(&capture_root, capture_context.clone())
                .map_err(route_error)?;
            for leaf in inventory.leaves() {
                let mut reader =
                    RovoDevSourceBackedReader::new(leaf, capture_context.clone(), None)
                        .map_err(route_error)?;
                sink.begin(leaf.source_key().clone())?;
                while let Some(page) = reader.next_page().map_err(route_error)? {
                    for document in page.documents {
                        sink.document(document)?;
                    }
                }
                let scan = reader.finish().map_err(route_error)?;
                if scan.disposition == RovoDevSourceBackedDisposition::Unchanged {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "cold Rovo Dev coordinator scan reported unchanged",
                    ));
                }
                sink.certify(scan.source)?;
            }
            inventory.certify().map_err(route_error)?;
            Ok(())
        },
        provider_format_scope(CaptureProvider::RovoDev, "rovodev_session_json_tree"),
        move |request| {
            let inventory =
                discover_rovodev_source_backed(&hydration_root, hydration_context.clone())
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?;
            let hydrated =
                hydrate_rovodev_source_record(&inventory, request.event_id(), request.locator())
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                    })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_trae_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let driver = captured_route_driver(
        move |sink| {
            let mut began = false;
            let mut sink_failure = None;
            let scan = scan_trae_source_backed_explicit_v0(&capture_path, &mut |page| {
                for document in page.documents {
                    if !began {
                        if let Err(error) = sink.begin(document.source.clone()) {
                            let detail = error.to_string();
                            sink_failure = Some(error);
                            return Err(TraeSourceBackedErrorV0::Capture(
                                CaptureError::InvalidPayload(detail),
                            ));
                        }
                        began = true;
                    }
                    if let Err(error) = sink.document(document) {
                        let detail = error.to_string();
                        sink_failure = Some(error);
                        return Err(TraeSourceBackedErrorV0::Capture(
                            CaptureError::InvalidPayload(detail),
                        ));
                    }
                }
                Ok(())
            })
            .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            if !began {
                sink.begin(scan.source.observation().source().clone())?;
            }
            sink.certify(scan.source)
        },
        provider_format_scope(CaptureProvider::Trae, "trae_state_vscdb"),
        move |request| {
            let hydrated =
                hydrate_trae_source_backed_locator_v0(&hydration_path, request.locator()).map_err(
                    |error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error),
                )?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.exact_text.into_bytes(),
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::ExplicitPath,
        driver,
    )?);
    Ok(())
}

fn register_openclaw_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let adapter = openclaw_source_backed_adapter_v0();
            for selected in adapter
                .discover_selected(&capture_root)
                .map_err(route_error)?
            {
                sink.begin(selected.source_key().clone())?;
                let mut reader = adapter
                    .open_source(&selected, DateTime::<Utc>::UNIX_EPOCH, None)
                    .map_err(route_error)?;
                while let Some(page) = reader.next_page().map_err(route_error)? {
                    for document in page.documents {
                        sink.document(document)?;
                    }
                }
                sink.certify(reader.finish().map_err(route_error)?.certified_source)?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::OpenClaw, "openclaw_session_jsonl_tree"),
        move |request| {
            let adapter = openclaw_source_backed_adapter_v0();
            for selected in adapter
                .discover_selected(&hydration_root)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
            {
                if selected
                    .source_key()
                    .exact_descriptor_eq(request.locator().source())
                {
                    let hydrated =
                        adapter
                            .hydrate(&selected, request.locator())
                            .map_err(|error| {
                                hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                            })?;
                    return Ok(HydratedProviderRecord {
                        event_id: request.event_id(),
                        provider_bytes: hydrated.provider_bytes,
                    });
                }
            }
            Err(hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "the exact OpenClaw source is absent from the selected inventory",
            ))
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_continue_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let discovery = discover_continue_root(&capture_root).map_err(route_error)?;
            let mut reader = ContinueSourceBackedReader::new(&discovery).map_err(route_error)?;
            let mut begun = HashSet::new();
            while let Some(outcome) = reader.next_outcome().map_err(route_error)? {
                match outcome {
                    ContinueSourceBackedOutcome::Page(page) => {
                        if let Some(document) = page.documents.first() {
                            if begun.insert(document.source.identity().digest()) {
                                sink.begin(document.source.clone())?;
                            }
                        }
                        for document in page.documents {
                            sink.document(document)?;
                        }
                        if let Some(terminal) = page.terminal {
                            if begun.insert(terminal.source.identity().digest()) {
                                sink.begin(terminal.source)?;
                            }
                            sink.certify(terminal.certificate)?;
                        }
                    }
                    ContinueSourceBackedOutcome::Incomplete(_) => {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Unavailable,
                            "Continue selected source was incomplete",
                        ));
                    }
                    ContinueSourceBackedOutcome::Failed(_) => {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Unavailable,
                            "Continue selected source failed during bounded discovery",
                        ));
                    }
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Continue, "continue_cli_sessions_json"),
        move |request| {
            let hydrated =
                hydrate_continue_source_backed_record(&hydration_root, request.locator()).map_err(
                    |error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error),
                )?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_mux_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root;
    let driver = captured_route_driver(
        move |sink| {
            for candidate in
                discover_mux_source_backed_sources(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                    .map_err(route_error)?
            {
                sink.begin(candidate.source_key().clone())?;
                let receipt = scan_mux_source_backed(&candidate, None, |page| {
                    for record in page.records {
                        sink.document(record.document).map_err(|error| {
                            super::providers::mux::native_path::MuxSourceBackedError::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })?;
                    }
                    Ok(())
                })
                .map_err(route_error)?;
                if !matches!(receipt.disposition, MuxSourceBackedDisposition::Cold) {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "cold Mux coordinator scan returned a non-cold disposition",
                    ));
                }
                sink.certify(receipt.certificate)?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Mux, "mux_session_jsonl"),
        |_request| {
            Err(hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Mux exact content requires its brokered compound-file resolver route",
            ))
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn register_direct_jsonl_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = direct_jsonl_adapter(source.provider).ok_or_else(|| {
        invalid_route(
            source.provider,
            "provider is not a member of the direct native-JSONL adapter family",
        )
    })?;
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let provider = source.provider;
    let certified_source_format = adapter.source_format();
    let driver = captured_route_driver(
        move |sink| capture_direct_jsonl(adapter, &capture_root, sink),
        provider_format_scope(provider, certified_source_format),
        move |request| hydrate_direct_jsonl(adapter, &hydration_root, request),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn direct_jsonl_adapter(provider: CaptureProvider) -> Option<DirectJsonlSourceAdapter> {
    match provider {
        CaptureProvider::Antigravity => Some(antigravity_source_backed_adapter()),
        CaptureProvider::CopilotCli => Some(copilot_source_backed_adapter()),
        CaptureProvider::FactoryAiDroid => Some(factory_droid_source_backed_adapter()),
        CaptureProvider::Qoder => Some(qoder_source_backed_adapter()),
        CaptureProvider::QwenCode => Some(qwen_code_source_backed_adapter()),
        CaptureProvider::Tabnine => Some(tabnine_source_backed_adapter()),
        CaptureProvider::Windsurf => Some(windsurf_source_backed_adapter()),
        _ => None,
    }
}

fn capture_direct_jsonl(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    sink: &mut dyn ProviderCaptureSink,
) -> SourceBackedRouteResult<()> {
    let inventory = adapter.discover(root).map_err(route_error)?;
    if inventory.root_missing() {
        return Ok(());
    }
    if !inventory.failures().is_empty() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "direct JSONL inventory contains inaccessible sources",
        ));
    }
    for leaf in inventory.leaves() {
        let mut reader = adapter
            .open_leaf(leaf, DateTime::<Utc>::UNIX_EPOCH)
            .map_err(route_error)?;
        let mut began = false;
        while let Some(page) = reader.next_page().map_err(route_error)? {
            if !began {
                sink.begin(page.source.clone())?;
                began = true;
            }
            for document in page.documents {
                sink.document(document)?;
            }
        }
        let certified = reader.finish().map_err(route_error)?;
        if !began {
            sink.begin(certified.source().clone())?;
        }
        sink.certify(certified.certificate().clone())?;
    }
    Ok(())
}

fn hydrate_direct_jsonl(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let inventory = adapter
        .discover(root)
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    for leaf in inventory.leaves() {
        let mut reader = adapter
            .open_leaf(leaf, DateTime::<Utc>::UNIX_EPOCH)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
        while let Some(_) = reader.next_page().map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })? {}
        let certified: DirectJsonlCertifiedLeaf = reader.finish().map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        if certified
            .source()
            .exact_descriptor_eq(request.locator().source())
        {
            let provider_bytes =
                adapter
                    .hydrate(&certified, request.locator())
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                    })?;
            return Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            });
        }
    }
    Err(hydration_failure(
        HydrationFailureKind::ConfirmedDeleted,
        "the exact direct JSONL source is absent from the complete inventory",
    ))
}

fn provider_format_scope(
    provider: CaptureProvider,
    source_format: &'static str,
) -> impl Fn(&SourceKey) -> bool + Send + Sync + 'static {
    move |source| source.provider() == provider.as_str() && source.source_format() == source_format
}

fn executable_route(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    authority: SourceBackedSelectorAuthority,
    driver: SourceBackedRouteDriver,
) -> SourceBackedCoordinatorResult<SourceBackedRoute> {
    match selection {
        SourceBackedRouteSelection::Automatic => {
            SourceBackedRoute::automatic(source, authority, driver)
        }
        SourceBackedRouteSelection::ExplicitManual => SourceBackedRoute::explicit_manual(
            source,
            if authority == SourceBackedSelectorAuthority::DiscoveredWinner {
                SourceBackedSelectorAuthority::ExplicitPath
            } else {
                authority
            },
            driver,
        ),
    }
}

fn validate_executable_route(
    source: &ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<&'static SourceBackedProviderRouteMetadata> {
    let known = landed_format_route(source.provider, source.source_format);
    let Some(known) = known else {
        return Err(invalid_route(
            source.provider,
            format!(
                "source format {:?} has no landed route",
                source.source_format
            ),
        ));
    };
    let selected_mode_supported = match selection {
        SourceBackedRouteSelection::Automatic => known.automatic,
        SourceBackedRouteSelection::ExplicitManual => known.explicit_manual,
    };
    if !selected_mode_supported
        || known.unsupported_reason.is_some()
        || !source.import_support.is_importable()
        || source.source_kind == ProviderSourceKind::DetectionOnly
        || source.status == ProviderSourceStatus::Unsupported
        || source.unsupported_reason.is_some()
    {
        return Err(invalid_route(
            source.provider,
            source
                .unsupported_reason
                .or(known.unsupported_reason)
                .unwrap_or("the selected automatic/manual mode is unsupported"),
        ));
    }
    if selection == SourceBackedRouteSelection::Automatic
        && source.import_support != ProviderImportSupport::Native
    {
        return Err(invalid_route(
            source.provider,
            "an explicit-only provider source cannot be registered automatically",
        ));
    }
    if selector_authority != known.selector_authority
        && !matches!(
            (selection, selector_authority),
            (
                SourceBackedRouteSelection::ExplicitManual,
                SourceBackedSelectorAuthority::ExplicitPath
            )
        )
    {
        return Err(invalid_route(
            source.provider,
            "the route omitted or changed its provider selector authority",
        ));
    }
    Ok(known)
}

fn landed_format_route(
    provider: CaptureProvider,
    selected_source_format: &str,
) -> Option<&'static SourceBackedProviderRouteMetadata> {
    LANDED_SOURCE_BACKED_ROUTES
        .iter()
        .find(|route| route.provider == provider && route.source_format == selected_source_format)
}

fn invalid_route(
    provider: CaptureProvider,
    detail: impl Into<String>,
) -> SourceBackedCoordinatorError {
    SourceBackedCoordinatorError::InvalidRoute {
        provider,
        detail: detail.into(),
    }
}

fn scan_gemini_route(
    root: &Path,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<()> {
    let discovery = discover_gemini_transcripts(root).map_err(route_capture_error)?;
    if !discovery.completed_inventory {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "Gemini discovery did not produce a complete inventory",
        ));
    }
    for source in &discovery.transcripts {
        let mut reader = GeminiSourceBackedLeafReader::open(source).map_err(route_error)?;
        sink.begin_source(reader.source().clone())
            .map_err(route_coordinator_error)?;
        while let Some(page) = reader.next_page().map_err(route_error)? {
            for document in page.documents {
                sink.add_document(document)
                    .map_err(route_coordinator_error)?;
            }
        }
        let leaf = reader.finish().map_err(route_error)?;
        sink.certify_source(leaf.certificate)
            .map_err(route_coordinator_error)?;
    }
    Ok(())
}

fn revalidate_gemini_source(root: &Path, expected: &CertifiedSource) -> bool {
    let Ok(discovery) = discover_gemini_transcripts(root) else {
        return false;
    };
    if !discovery.completed_inventory {
        return false;
    }
    for source in &discovery.transcripts {
        let Ok(mut reader) = GeminiSourceBackedLeafReader::open(source) else {
            return false;
        };
        if !reader
            .source()
            .exact_descriptor_eq(expected.observation().source())
        {
            continue;
        }
        while let Ok(Some(_)) = reader.next_page() {}
        return reader
            .finish()
            .is_ok_and(|leaf| leaf.certificate == *expected);
    }
    false
}

fn hydrate_gemini_route(
    root: &Path,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let discovery = discover_gemini_transcripts(root)
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    if !discovery.completed_inventory {
        return Err(hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "Gemini discovery did not complete",
        ));
    }
    for source in &discovery.transcripts {
        let reader = GeminiSourceBackedLeafReader::open(source).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        let owned = reader
            .source()
            .exact_descriptor_eq(request.locator().source());
        drop(reader);
        if owned {
            let hydrated = hydrate_gemini_source_backed_record(source, request.locator()).map_err(
                |error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error),
            )?;
            return Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            });
        }
    }
    Err(hydration_failure(
        HydrationFailureKind::ConfirmedDeleted,
        "the exact Gemini source is absent from the complete inventory",
    ))
}

struct CursorGenerationBridge<'sink, 'writer> {
    sink: &'sink mut SourceBackedGenerationSink<'writer>,
    active: Option<SourceKey>,
}

impl CursorSourceBackedSink for CursorGenerationBridge<'_, '_> {
    fn begin_cursor_source(&mut self, plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        self.sink
            .begin_source(plan.source.clone())
            .map_err(capture_coordinator_error)?;
        self.active = Some(plan.source.clone());
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> CaptureResult<()> {
        for record in page.records {
            if let Some(document) = record.lexical_document() {
                self.sink
                    .add_document(document)
                    .map_err(capture_coordinator_error)?;
            }
        }
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        let active = self.active.take().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Cursor source-backed terminal arrived without an active source".to_owned(),
            )
        })?;
        if !active.exact_descriptor_eq(terminal.certified_source.observation().source()) {
            return Err(CaptureError::InvalidPayload(
                "Cursor source-backed terminal changed its active source".to_owned(),
            ));
        }
        self.sink
            .certify_source(terminal.certified_source)
            .map_err(capture_coordinator_error)
    }

    fn abort_cursor_source(&mut self) {
        self.active = None;
    }
}

fn scan_cursor_route(
    root: &Path,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<()> {
    let mut bridge = CursorGenerationBridge { sink, active: None };
    extract_cursor_source_backed_cold(root, &mut bridge).map_err(route_capture_error)?;
    if bridge.active.is_some() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Cursor extraction ended with an uncertified active source",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct CursorEvidenceSink {
    certificates: Vec<CertifiedSource>,
}

impl CursorSourceBackedSink for CursorEvidenceSink {
    fn begin_cursor_source(&mut self, _plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, _page: CursorSourceBackedPage) -> CaptureResult<()> {
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        self.certificates.push(terminal.certified_source);
        Ok(())
    }

    fn abort_cursor_source(&mut self) {}
}

fn revalidate_cursor_source(root: &Path, expected: &CertifiedSource) -> bool {
    let mut sink = CursorEvidenceSink::default();
    if extract_cursor_source_backed_cold(root, &mut sink).is_err() {
        return false;
    }
    sink.certificates.into_iter().any(|certificate| {
        certificate
            .observation()
            .source()
            .exact_descriptor_eq(expected.observation().source())
            && certificate == *expected
    })
}

struct CursorHydrationSink<'request> {
    request: &'request EventHydrationRequest,
    record: Option<CursorSourceBackedRecord>,
}

impl CursorSourceBackedSink for CursorHydrationSink<'_> {
    fn begin_cursor_source(&mut self, _plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> CaptureResult<()> {
        for record in page.records {
            if record.event_id == self.request.event_id()
                && record.locator == *self.request.locator()
            {
                if self.record.replace(record).is_some() {
                    return Err(CaptureError::InvalidPayload(
                        "Cursor exact locator resolved more than once".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn finish_cursor_source(&mut self, _terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        Ok(())
    }

    fn abort_cursor_source(&mut self) {}
}

fn hydrate_cursor_route(
    root: &Path,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let mut sink = CursorHydrationSink {
        request,
        record: None,
    };
    extract_cursor_source_backed_cold(root, &mut sink)
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    let record = sink.record.ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Cursor exact locator is absent from the selected transcript tree",
        )
    })?;
    let text = hydrate_cursor_source_backed_message(root, &record)
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
    Ok(HydratedProviderRecord {
        event_id: request.event_id(),
        provider_bytes: text.into_bytes(),
    })
}

fn route_capture_error(error: CaptureError) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
}

fn route_error(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn route_coordinator_error(error: SourceBackedCoordinatorError) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn capture_coordinator_error(error: SourceBackedCoordinatorError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ctx_history_core::{
        derive_event_id, derive_session_id, EventIdentityInput, LocatorRevisionPolicy,
        NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts,
        SessionIdentityInput, SourceAnchor, SourceObservation, SourceRecordLocator, TypedKey,
    };
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn heterogeneous_routes_publish_once_and_hydrate_exact_locators() {
        let gemini = fixture_route(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            1,
            NativeRecordCoordinate::Jsonl {
                byte_offset: 10,
                byte_length: 4,
                physical_ordinal: 1,
                native_session_key: None,
                native_event_key: None,
            },
            b"gemini".to_vec(),
        );
        let hermes = fixture_route(
            CaptureProvider::Hermes,
            "hermes_state_sqlite",
            2,
            NativeRecordCoordinate::ProviderSqlite {
                logical_relation: "messages".to_owned(),
                primary_key: TypedKey::I64(7),
                row_version: None,
            },
            b"hermes".to_vec(),
        );
        let gemini_request = gemini.1.clone();
        let hermes_request = hermes.1.clone();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(gemini.0);
        registry.register(hermes.0);

        let temp = tempdir().unwrap();
        let receipt = refresh_source_backed_generation(
            temp.path(),
            &registry,
            WriterOptions {
                indexer_threads: 1,
                memory_bytes: 15_000_000,
            },
        )
        .unwrap();
        assert_eq!(receipt.scanned_routes, 2);
        assert_eq!(receipt.commit.indexed_documents, 2);
        assert_eq!(receipt.commit.certified_sources, 2);

        let resolver = registry.resolver_registry();
        assert_eq!(
            resolver
                .hydrate_event(&gemini_request)
                .unwrap()
                .provider_bytes,
            b"gemini"
        );
        assert_eq!(
            resolver
                .hydrate_event(&hermes_request)
                .unwrap()
                .provider_bytes,
            b"hermes"
        );
    }

    #[test]
    fn unsupported_detected_format_stays_typed_and_never_claims_a_locator() {
        let source = fixture_provider_source(
            CaptureProvider::Unknown,
            "unknown_detected_format",
            ProviderImportSupport::Unsupported,
        );
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(SourceBackedRoute::unsupported(
            source,
            "no product-approved source-backed adapter",
        ));
        assert!(matches!(
            refresh_source_backed_generation(
                tempdir().unwrap().path(),
                &registry,
                WriterOptions::default()
            ),
            Err(SourceBackedCoordinatorError::NoExecutableRoutes)
        ));
    }

    #[test]
    fn importable_provider_inventory_covers_default_and_explicit_formats() {
        assert_eq!(LANDED_SOURCE_BACKED_ROUTES.len(), 52);
        assert_eq!(
            LANDED_SOURCE_BACKED_ROUTES
                .iter()
                .filter(|route| route.automatic)
                .count(),
            41
        );
        assert_eq!(
            LANDED_SOURCE_BACKED_ROUTES
                .iter()
                .filter(|route| route.automatic && route.unsupported_reason.is_some())
                .count(),
            1
        );
        let mut formats = HashSet::new();
        for route in LANDED_SOURCE_BACKED_ROUTES {
            assert!(
                formats.insert((route.provider, route.source_format)),
                "{} {} is registered more than once",
                route.provider.as_str(),
                route.source_format
            );
            assert!(!route.source_format.is_empty());
            assert!(!route.certified_source_format.is_empty());
            match route.exact_hydration {
                SourceBackedHydrationSupport::Full => {
                    assert!(route.hydration_limitation.is_none());
                    assert!(route.unsupported_reason.is_none());
                }
                SourceBackedHydrationSupport::Partial => {
                    assert!(route.hydration_limitation.is_some());
                    assert!(route.unsupported_reason.is_none());
                }
                SourceBackedHydrationSupport::Unsupported => {
                    assert!(route.unsupported_reason.is_some());
                }
            }
        }

        for spec in crate::provider_source_specs()
            .iter()
            .filter(|spec| spec.import_support.is_importable())
        {
            let routes = LANDED_SOURCE_BACKED_ROUTES
                .iter()
                .filter(|route| route.provider == spec.provider)
                .collect::<Vec<_>>();
            assert!(
                !routes.is_empty(),
                "{} must have at least one central source-backed format route",
                spec.provider.as_str()
            );
            assert!(
                source_backed_route_constructor(spec.provider).is_some(),
                "{} must have a mechanical driver constructor",
                spec.provider.as_str()
            );
            for location in spec.default_locations {
                let matching = routes
                    .iter()
                    .filter(|route| route.source_format == location.source_format)
                    .collect::<Vec<_>>();
                assert_eq!(
                    matching.len(),
                    1,
                    "{} default format {} must have exactly one central format route",
                    spec.provider.as_str(),
                    location.source_format
                );
                if spec.import_support == ProviderImportSupport::Native {
                    assert!(
                        matching[0].automatic,
                        "{} default format {} is not automatic",
                        spec.provider.as_str(),
                        location.source_format
                    );
                }
            }
        }

        let root_leaf_variants = [
            (
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::Codex,
                "codex_history_jsonl",
                "codex_history_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::Codex,
                "codex_session_jsonl",
                "codex_session_jsonl",
                false,
                true,
            ),
            (
                CaptureProvider::Cursor,
                "cursor_agent_transcript_jsonl_tree",
                "cursor_agent_transcript_jsonl_tree",
                true,
                true,
            ),
            (
                CaptureProvider::Cursor,
                "cursor_agent_transcript_jsonl",
                "cursor_agent_transcript_jsonl_tree",
                false,
                true,
            ),
            (
                CaptureProvider::Windsurf,
                "windsurf_cascade_hook_transcript_jsonl_tree",
                "windsurf_cascade_hook_transcript_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::Windsurf,
                "windsurf_cascade_hook_transcript_jsonl",
                "windsurf_cascade_hook_transcript_jsonl",
                false,
                true,
            ),
            (
                CaptureProvider::QwenCode,
                "qwen_code_chat_jsonl_tree",
                "qwen_code_chat_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::QwenCode,
                "qwen_code_chat_jsonl",
                "qwen_code_chat_jsonl",
                false,
                true,
            ),
            (
                CaptureProvider::KimiCodeCli,
                "kimi_code_cli_wire_jsonl_tree",
                "kimi_code_cli_wire_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::KimiCodeCli,
                "kimi_code_cli_wire_jsonl",
                "kimi_code_cli_wire_jsonl",
                false,
                true,
            ),
            (
                CaptureProvider::MistralVibe,
                "mistral_vibe_session_jsonl_tree",
                "mistral_vibe_session_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::MistralVibe,
                "mistral_vibe_session_jsonl",
                "mistral_vibe_session_jsonl",
                false,
                true,
            ),
            (
                CaptureProvider::Mux,
                "mux_session_jsonl_tree",
                "mux_session_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::Mux,
                "mux_session_jsonl",
                "mux_session_jsonl",
                false,
                true,
            ),
            (
                CaptureProvider::Qoder,
                "qoder_transcript_jsonl_tree",
                "qoder_transcript_jsonl",
                true,
                true,
            ),
            (
                CaptureProvider::Qoder,
                "qoder_transcript_jsonl",
                "qoder_transcript_jsonl",
                false,
                true,
            ),
            (
                CaptureProvider::Junie,
                "junie_session_events_jsonl",
                "junie_session_events_jsonl_tree",
                false,
                true,
            ),
        ];
        for (provider, selected, certified, automatic, explicit) in root_leaf_variants {
            let route = landed_format_route(provider, selected).unwrap();
            assert_eq!(route.certified_source_format, certified);
            assert_eq!(route.automatic, automatic);
            assert_eq!(route.explicit_manual, explicit);
        }
    }

    #[test]
    fn automatic_builder_counts_routes_and_returns_typed_gaps() {
        let context = DiscoveryContext::new(
            "/home/test",
            "/work/test",
            DiscoveryPlatform::Linux,
            crate::DiscoveryPlatformDirs {
                state: Some(PathBuf::from("/state")),
                ..crate::DiscoveryPlatformDirs::default()
            },
        );
        let mut missing_mux = fixture_provider_source(
            CaptureProvider::Mux,
            "mux_session_jsonl_tree",
            ProviderImportSupport::Native,
        );
        missing_mux.exists = false;
        missing_mux.status = ProviderSourceStatus::Missing;
        let sources = vec![
            fixture_provider_source(
                CaptureProvider::Gemini,
                GEMINI_CLI_SOURCE_FORMAT,
                ProviderImportSupport::Native,
            ),
            fixture_provider_source_at(
                CaptureProvider::Warp,
                "warp_sqlite",
                ProviderImportSupport::Native,
                "/state/warp-terminal/warp.sqlite",
            ),
            fixture_provider_source_at(
                CaptureProvider::Goose,
                "goose_sessions_sqlite",
                ProviderImportSupport::Native,
                "/home/test/.local/share/goose/sessions/sessions.db",
            ),
            fixture_provider_source(
                CaptureProvider::AstrBot,
                "astrbot_data_v4_sqlite",
                ProviderImportSupport::Native,
            ),
            fixture_provider_source_at(
                CaptureProvider::AstrBot,
                "astrbot_data_v4_sqlite",
                ProviderImportSupport::Native,
                "/home/test/.astrbot_launcher/instances/one/data/data_v4.db",
            ),
            fixture_provider_source(
                CaptureProvider::Codex,
                "codex_history_jsonl",
                ProviderImportSupport::Native,
            ),
            fixture_provider_source(
                CaptureProvider::Crush,
                "crush_sqlite",
                ProviderImportSupport::Native,
            ),
            fixture_provider_source(
                CaptureProvider::Lingma,
                "lingma_sqlite",
                ProviderImportSupport::Native,
            ),
            fixture_provider_source(
                CaptureProvider::Unknown,
                "unknown_detected_format",
                ProviderImportSupport::Unsupported,
            ),
            missing_mux,
        ];

        let build =
            build_automatic_source_backed_registry_from_report(&context, sources, Vec::new());
        assert_eq!(build.executable_route_count(), 3);
        assert_eq!(build.unsupported_route_count(), 5);
        assert_eq!(build.issues.len(), 6);
        assert!(build.issues.iter().any(|issue| matches!(
            issue,
            SourceBackedAutomaticRegistryIssue::Unavailable {
                source,
                reason: SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail },
            } if source.provider == CaptureProvider::Codex
                && source.source_format == "codex_history_jsonl"
                && detail.contains("prompt-history")
        )));
        assert!(build.issues.iter().any(|issue| matches!(
            issue,
            SourceBackedAutomaticRegistryIssue::Unavailable {
                source,
                reason:
                    SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                        detail,
                    },
            } if source.provider == CaptureProvider::Crush
                && detail.contains("stable project keys")
        )));
        assert!(build.issues.iter().any(|issue| matches!(
            issue,
            SourceBackedAutomaticRegistryIssue::Unavailable {
                source,
                reason: SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. },
            } if source.provider == CaptureProvider::Unknown
                && source.source_format == "unknown_detected_format"
        )));
    }

    #[test]
    fn resolver_routes_selected_tree_to_certified_leaf_format() {
        let route = fixture_route_with_selected_format(
            CaptureProvider::Qoder,
            "qoder_transcript_jsonl_tree",
            "qoder_transcript_jsonl",
            8,
            NativeRecordCoordinate::Jsonl {
                byte_offset: 3,
                byte_length: 5,
                physical_ordinal: 1,
                native_session_key: None,
                native_event_key: None,
            },
            b"qoder".to_vec(),
        );
        let request = route.1.clone();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route.0);
        assert_eq!(
            registry
                .resolver_registry()
                .hydrate_event(&request)
                .unwrap()
                .provider_bytes,
            b"qoder"
        );
    }

    fn fixture_route(
        provider: CaptureProvider,
        source_format: &'static str,
        lineage: u8,
        coordinate: NativeRecordCoordinate,
        provider_bytes: Vec<u8>,
    ) -> (SourceBackedRoute, EventHydrationRequest) {
        fixture_route_with_selected_format(
            provider,
            source_format,
            source_format,
            lineage,
            coordinate,
            provider_bytes,
        )
    }

    fn fixture_route_with_selected_format(
        provider: CaptureProvider,
        selected_source_format: &'static str,
        certified_source_format: &'static str,
        lineage: u8,
        coordinate: NativeRecordCoordinate,
        provider_bytes: Vec<u8>,
    ) -> (SourceBackedRoute, EventHydrationRequest) {
        let source = SourceKey::derive(
            provider.as_str(),
            certified_source_format,
            "coordinator-test-v1",
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap();
        let session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "session",
            native_session_key: &session_key,
        })
        .unwrap();
        let item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let revision_digest = [lineage.saturating_add(10); 32];
        let record_digest = [lineage.saturating_add(20); 32];
        let locator = SourceRecordLocator::new(
            source.clone(),
            coordinate,
            LocatorRevisionPolicy::ExactSourceRevision,
            Some(revision_digest),
            record_digest,
        )
        .unwrap();
        let request = EventHydrationRequest::new(event_id, locator.clone()).unwrap();
        let document = LexicalDocument {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            source: source.clone(),
            locator,
            provider_session_id: Some("session".to_owned()),
            branch: None,
            source_path: Some(format!("/fixture/{}", provider.as_str())),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: 1,
            occurred_at_unix_ms: Some(1),
            event_type: "message".to_owned(),
            role: Some("user".to_owned()),
            body: format!("{} preview", provider.as_str()),
            workspace: None,
            cwd: None,
            touched_files: Vec::new(),
        };
        let observation =
            SourceObservation::new(source.clone(), "fixture-revision", vec![lineage]).unwrap();
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            "coordinator-test-v1",
            revision_digest,
            ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                indexed_documents: 1,
                certified_bytes: 1,
                ..ScannedSourceCounts::default()
            },
        )
        .unwrap();
        let scan_certificate = certificate.clone();
        let scan_document = document.clone();
        let owned_source = source.clone();
        let revalidation_certificate = certificate;
        let hydrated_bytes = provider_bytes;
        let driver = SourceBackedRouteDriver::new(
            move |sink| {
                sink.replace_source(scan_certificate.clone(), [scan_document.clone()])
                    .map_err(route_coordinator_error)
            },
            move |candidate| candidate.exact_descriptor_eq(&owned_source),
            move |target| match target {
                SourceBackedRevalidationTarget::Source(source) => {
                    source == &revalidation_certificate
                }
                SourceBackedRevalidationTarget::Deletion(_) => false,
            },
            move |request| {
                Ok(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes: hydrated_bytes.clone(),
                })
            },
        );
        let provider_source = fixture_provider_source(
            provider,
            selected_source_format,
            ProviderImportSupport::Native,
        );
        (
            SourceBackedRoute::automatic(
                provider_source,
                SourceBackedSelectorAuthority::DiscoveredWinner,
                driver,
            )
            .unwrap(),
            request,
        )
    }

    fn fixture_provider_source(
        provider: CaptureProvider,
        source_format: &'static str,
        import_support: ProviderImportSupport,
    ) -> ProviderSource {
        ProviderSource {
            provider,
            path: PathBuf::from(format!("/fixture/{}", provider.as_str())),
            exists: true,
            source_format,
            source_kind: if import_support == ProviderImportSupport::Unsupported {
                ProviderSourceKind::DetectionOnly
            } else {
                ProviderSourceKind::NativeHistory
            },
            import_support,
            catalog_support: crate::ProviderCatalogSupport::None,
            status: if import_support == ProviderImportSupport::Unsupported {
                ProviderSourceStatus::Unsupported
            } else {
                ProviderSourceStatus::Available
            },
            unsupported_reason: None,
        }
    }

    fn fixture_provider_source_at(
        provider: CaptureProvider,
        source_format: &'static str,
        import_support: ProviderImportSupport,
        path: impl Into<PathBuf>,
    ) -> ProviderSource {
        let mut source = fixture_provider_source(provider, source_format, import_support);
        source.path = path.into();
        source
    }
}

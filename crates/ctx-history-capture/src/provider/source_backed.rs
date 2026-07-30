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
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, CaptureProvider, CertifiedSource,
    CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    ContentSourceResolver, EventHydrationRequest, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, ScannedSourceCounts, SourceAnchor, SourceFrontier, SourceKey, TypedKey,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, LexicalDocument, RevalidationTarget, WriterOptions,
};
use thiserror::Error;

use super::codex::nativepath::{
    codex_source_observation, codex_writer_base_sources, discover_codex_root_inventory_v0,
    ingest_codex_sources_serial_v0, managed_codex_session_source,
    observe_codex_explicit_session_source_backed_v0,
    observe_codex_prompt_history_source_backed_explicit_v0,
    scan_codex_prompt_history_source_backed_v0, CodexExplicitSessionSourceBackedInputV0,
    CodexHydratedRecordV0, CodexLocatorResolverV0, CodexPromptHistorySourceBackedDispositionV0,
    CodexPromptHistorySourceBackedInputV0, CodexPromptHistorySourceBackedResolverV0,
    CodexSourceBackedCountersV0, CodexSourceBackedErrorV0, CodexSourceBackedPhaseTimingsV0,
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
    continue_cli::native_path::{ContinueSourceBackedOutcome, ContinueSourceBackedReader},
    crush::native_path::source_backed::{
        bind_inventory as bind_crush_inventory, closing_observation as closing_crush_observation,
        exact_replay_matches as crush_exact_replay_matches,
        finish_opened_source as finish_crush_source, open_source as open_crush_source,
        scan_source as scan_crush_source, CrushLocatorResolverV0, CrushSourceBackedErrorV0,
        CrushSourceBackedResultV0, CRUSH_DISCOVERY_REVISION, CRUSH_FRONTIER_KIND,
        CRUSH_PARSER_REVISION, CRUSH_SOURCE_SCHEMA_VARIANT,
    },
    cursor::{
        extract_cursor_source_backed_cold, hydrate_cursor_source_backed_message,
        CursorSourceBackedPage, CursorSourceBackedRecord, CursorSourceBackedSink,
        CursorSourceBackedSourcePlan, CursorSourceBackedTerminal,
    },
    deepagents::native_path::source_backed::{
        DeepAgentsDatabaseSelectionV0, DeepAgentsLocatorResolverV0, DeepAgentsSourceBackedScannerV0,
    },
    forgecode::nativepath::source_backed::{
        open_forgecode_source_backed_v0, ForgeCodeSourceBackedDiscoveryV0,
        ForgeCodeSourceBackedResolverV0, ForgeCodeSourceSelectionV0,
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
    lingma::native_path::{
        scan_lingma_source_backed_v0, LingmaDatabaseSourceV0, LingmaSourceBackedErrorV0,
        LingmaSourceBackedResolverV0, LingmaSourceBackedResultV0, LingmaSourceInventoryV0,
    },
    mistral_vibe::native_path::source_backed::scan_mistral_vibe_source_backed,
    mux::native_path::{
        discover_mux_source_backed_sources, scan_mux_source_backed, MuxSourceBackedDisposition,
        MuxSourceBackedResolverV0,
    },
    nanoclaw::native_path::source_backed::{
        hydrate_nanoclaw_source_backed_exact, nanoclaw_source_key, scan_nanoclaw_source_backed,
    },
    openclaw::openclaw_source_backed_adapter_v0,
    openhands::nativepath::{
        openhands_owns_source, openhands_route_error, OpenHandsEventFileAdapterV2,
        OpenHandsEventFileSourcePlan,
    },
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
use crate::provider_sources::{
    resolve_warp_discovery_authority, CrushDiscoveredProjectInventory,
    CrushProjectInventorySelector, CrushProjectInventorySelectorError, LingmaDiscoveryUnavailable,
    LingmaInventorySelector, WarpDiscoveryUnavailable,
};
use crate::{
    discover_provider_sources_with_context, provider_source_spec, CaptureError, DiscoveryContext,
    DiscoveryIssue, DiscoveryPlatform, ProviderAdapterContext, ProviderImportSupport,
    ProviderSource, ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus,
    Result as CaptureResult, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
};

mod discovery;
mod driver;
pub(crate) mod family;
mod inventory;
mod publication;
mod registration;
mod resolver;

pub use discovery::*;
pub use driver::*;
const _: Option<&dyn driver::ProviderCaptureSink> = None;
pub use inventory::*;
pub use publication::*;
pub use registration::*;
pub use resolver::*;

pub(crate) fn source_backed_base_sources(
    sink: &SourceBackedGenerationSink<'_>,
    owns: impl Fn(&SourceKey) -> bool,
) -> Vec<CertifiedSource> {
    sink.writer
        .base_manifest()
        .map(|manifest| {
            manifest
                .sources
                .iter()
                .filter(|source| owns(source.observation().source()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;

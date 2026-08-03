//! Shared coordination for provider-owned source-backed projections.
//!
//! Provider adapters remain responsible for discovery, parsing, projection,
//! and source certification. This module owns the one production
//! publication boundary: all registered adapters stage into one
//! [`GenerationWriter`] and no adapter can publish a generation by itself.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, ScannedSourceCounts, SourceAnchor, SourceKey, TypedKey,
};
use ctx_history_index::{
    CommitReceipt, CoreRecordPreparer, GenerationManifest, GenerationWriter, IndexError,
    PublicationDisposition, PublicationMetadataContext, RevalidationTarget, SourceRouteIdentity,
    SourceRouteSnapshot, VerifiedIndex, WriterOptions,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::codex::nativepath::{
    codex_source_observation, ingest_codex_sources_serial_v0, managed_codex_session_source,
    observe_codex_explicit_session_source_backed_v0,
    observe_codex_prompt_history_source_backed_explicit_v0,
    plan_codex_prompt_history_source_backed_v0, revalidate_codex_prompt_history_source_backed_v0,
    scan_codex_prompt_history_source_backed_v0,
    stage_planned_codex_prompt_history_source_backed_v0, CodexExplicitSessionSourceBackedInputV0,
    CodexPromptHistorySourceBackedDispositionV0, CodexPromptHistorySourceBackedErrorV0,
    CodexPromptHistorySourceBackedInputV0, CodexSourceBackedCountersV0, CodexSourceBackedErrorV0,
    CodexSourceBackedPhaseTimingsV0, CodexTerminalSourceEvidenceV0,
};
use super::custom_history_jsonl::{
    observe_custom_history_source_backed_explicit, revalidate_custom_history_source_backed,
    scan_custom_history_source_backed_explicit, CustomHistorySourceBackedDisposition,
    CustomHistorySourceBackedError, CustomHistorySourceBackedInput,
    CustomHistorySourceBackedOutcome,
};
pub use super::providers::crush::native_path::source_backed::{
    CrushProjectDatabaseV0, CrushProjectInventoryObservationV0, CrushProjectInventorySourceV0,
};
use super::providers::{
    astrbot::native_path::source_backed::{
        scan_astrbot_snapshot_v0, AstrBotSourceBackedInventoryV0, AstrBotSourceBackedSourceV0,
        PARSER_REVISION as ASTRBOT_SOURCE_BACKED_PARSER_REVISION,
    },
    continue_cli::native_path::{ContinueSourceBackedOutcome, ContinueSourceBackedReader},
    crush::native_path::source_backed::{
        bind_inventory as bind_crush_inventory, finish_opened_source as finish_crush_source,
        scan_source as scan_crush_source, CrushSourceBackedErrorV0, CrushSourceBackedResultV0,
        CRUSH_PARSER_REVISION,
    },
    deepagents::native_path::source_backed::DeepAgentsDatabaseSelectionV0,
    forgecode::nativepath::source_backed::ForgeCodeSourceSelectionV0,
    goose::{GooseSourceBackedAdapterV0, GooseSourceBackedSelectionV0, GooseSourceRouteV0},
    hermes::source_backed::{hermes_source_backed_explicit, HermesSourceCandidate},
    junie::nativepath::junie_jsonl_adapter,
    kimi::native_path::source_backed::{KimiSourceBackedCatalog, KimiSourceBackedResolver},
    lingma::native_path::{
        reject_duplicate_paths as reject_duplicate_lingma_paths, scan_lingma_snapshot_v0,
        LingmaDatabaseSourceV0, LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0,
        LingmaSourceInventoryV0, LINGMA_SOURCE_BACKED_PARSER_REVISION,
    },
    mistral_vibe::native_path::source_backed::scan_mistral_vibe_source_backed,
    mux::mux_jsonl_adapter,
    nanoclaw::native_path::source_backed::NanoClawDocumentTreeAdapter,
    openclaw::openclaw_source_backed_adapter_v0,
    openhands::nativepath::OpenHandsEventFileAdapterV2,
    pi::nativepath::{pi_source_backed_adapter, PiSourceBackedRoot},
    rovodev::native_path::RovoDevDocumentTreeAdapter,
    shelley::native_path::source_backed::{
        discover_shelley_source_backed_exact_cwd, ShelleySourceBackedAdapter,
        SHELLEY_SOURCE_PARSER_REVISION,
    },
    task_json::cline_nativepath::{
        cline_task_json_source_backed_adapter, roo_task_json_source_backed_adapter,
    },
    warp::{project_warp_source_backed_v0, WarpSourceSelectionV0},
    zed::native_path::source_backed::{
        acquire_snapshot as acquire_zed_snapshot, decode_sha256_hex as decode_zed_digest,
        scan_zed_native_snapshot, snapshot_revision_digest as zed_snapshot_revision_digest,
        source_observation as zed_source_observation, zed_source_key, ZedSourceBackedSinkV0,
        ZED_PARSER_REVISION,
    },
};
use crate::provider_sources::{
    path_presence, resolve_warp_discovery_authority, CrushDiscoveredProjectInventory,
    CrushProjectInventorySelector, CrushProjectInventorySelectorError, LingmaDiscoveryUnavailable,
    LingmaInventorySelector, PathPresence, WarpDiscoveryUnavailable,
};
use crate::{
    discover_provider_sources_with_context, provider_source_spec,
    validate_provider_source_roots_outside_data_root, CaptureError, DiscoveryContext,
    DiscoveryIssue, DiscoveryPlatform, DiscoveryReport, ProviderAdapterContext,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceSpec,
    ProviderSourceStatus,
};

mod discovery;
mod driver;
mod fallback_identity;
pub(crate) mod family;
mod inventory;
mod publication;
mod registration;
mod watch;

pub use discovery::*;
pub use driver::*;
pub(crate) use fallback_identity::{FallbackEventIdentityMode, FallbackEventIdentityState};
pub use inventory::*;
pub use publication::*;
pub use registration::*;
pub use watch::*;

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

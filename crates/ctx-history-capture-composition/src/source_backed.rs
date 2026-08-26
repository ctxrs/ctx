//! Shared coordination for provider-owned source-backed projections.
//!
//! Provider adapters remain responsible for discovery, parsing, projection,
//! and source certification. This module owns the one production
//! publication boundary: all registered adapters stage into one neutral
//! lifecycle and no adapter can publish a generation by itself.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::{
    provider_source_config_digest, ProviderRootDefinition, ProviderRootSourceIdentity,
    ProviderRouteRole, ProviderSourceRouteProvenance, RetainedProviderRootAuthority,
    SourceRouteIdentity,
};
use ctx_history_capture_runtime::{
    CaptureLifecycleSink, CapturePublicationContext, CapturePublicationDisposition,
    CaptureSourceAggregateRef, ImmutableCaptureSnapshot,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceInventory, SourceKey, TypedKey,
};
#[cfg(test)]
use ctx_history_core::{CertifiedSourceAppend, CertifiedSourceDeletion, SourceAnchor};
use ctx_history_index::{
    source_token, AppliedProviderRoot, AppliedProviderRootSourceMembership, IndexError,
    PublicationStage, WriterOptions,
};
use ctx_history_provider_mistral_mux::{
    mistral_vibe_jsonl_adapter_with_source_root_lineage, mux_jsonl_adapter_with_source_root_lineage,
};
use sha2::{Digest, Sha256};

use crate::{
    provider_source_spec, validate_provider_source_roots_outside_data_root, DiscoveryContext,
    DiscoveryIssue, DiscoveryPlatform, DiscoveryReport, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus, StaticProviderProbeCatalog,
    OPENHANDS_CURRENT_CLI_SOURCE_FORMAT,
};
use ctx_history_provider_codex::codex::nativepath::CodexGenerationNormalizationCoordinatorV0;
use ctx_history_provider_docproj::providers::{
    nanoclaw::native_path::source_backed::NanoClawDocumentTreeAdapter,
    openhands::nativepath::OpenHandsEventFileAdapterV2,
};
pub use ctx_history_providers_sqlite_inventory::registration::SqliteInventoryCoverage;
pub use ctx_history_providers_sqlite_inventory::{
    crush_source_key, lingma_source_key, CrushProjectDatabaseV0,
    CrushProjectInventoryObservationV0, CrushProjectInventorySourceV0,
};
use ctx_history_source_discovery::{
    path_presence, provider_paths_equivalent, provider_source_belongs_to_configured_root,
    resolve_crush_released_project_inventories, resolve_lingma_released_identity_authority,
    resolve_openhands_conversations_root, resolve_warp_discovery_authority,
    resolve_warp_released_identity_authority, CrushDiscoveredProjectInventory,
    CrushProjectInventorySelector, CrushProjectInventorySelectorError, LingmaInventorySelector,
    PathPresence, WarpDiscoveryUnavailable,
};

mod automatic_route_split;
mod discovery;
mod driver;
pub(crate) mod family;
mod inventory;
mod publication;
mod registration;
mod runtime_adapter;
mod watch;

pub use automatic_route_split::*;
pub use ctx_history_capture_runtime::{
    CaptureLifecycleOpenOutcome, CaptureRevalidationTarget, PresentCaptureRoute,
};
#[cfg(test)]
pub(crate) use family::jsonl::FallbackEventIdentityMode;
pub(crate) use family::jsonl::FallbackEventIdentityState;
#[doc(hidden)]
pub use family::{CaptureDocumentSpool, CaptureProviderRuntime};
#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
pub use runtime_adapter::automatic_route_deletion_missing_observations_for_test;
pub(crate) use runtime_adapter::*;
pub use runtime_adapter::{
    BorrowedIndexManifestView, CommittedIndexManifestView, IndexCaptureCommitReceipt,
    IndexCaptureLifecycle, IndexCaptureVerifiedPin, IndexManifestView, IndexVerifiedCapture,
};
pub use {discovery::*, driver::*, watch::*};
pub use {inventory::*, publication::*, registration::*};

#[cfg(test)]
const _: Option<FallbackEventIdentityMode> = None;
const _: Option<FallbackEventIdentityState> = None;

#[cfg(test)]
pub(crate) fn source_backed_base_sources(
    sink: &SourceBackedGenerationSink<'_>,
    owns: impl Fn(&SourceKey) -> bool,
) -> Vec<CertifiedSource> {
    sink.lifecycle
        .base_snapshot()
        .map(|manifest| {
            manifest
                .sources()
                .iter()
                .filter(|source| owns(source.observation().source()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;

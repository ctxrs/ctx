use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{
    derive_event_id, derive_native_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, ProjectionContractError, SourceAnchorScope,
    SourceKey, StableEntityId, TypedKey,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    discover_codex_catalog_sources,
    reader::{
        opened_file_prefix_sha256, reopen_codex_source_capability,
        revalidate_codex_catalog_source_capability,
    },
    rows::{
        CodexCoreRecordDraft, CodexProviderEventIdentityKindV0, CodexProviderEventIdentityV0,
        CodexProviderNativeEventCopyV0,
    },
    source::{CodexCatalogSource, CodexFileObservation},
    CodexNativeScanner, CodexSessionRow,
};
use crate::{
    common::io::{
        open_provider_source_file, OpenedProviderSourcePath, ProviderSourceRoot,
        PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
        PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
    },
    provider::codex::{
        catalog::catalog_codex_explicit_session_opened, nativepath::opened_codex_file_observation,
    },
    CaptureError, CODEX_SESSION_SOURCE_FORMAT,
};

const CODEX_SOURCE_ANCHOR_NAMESPACE: &str = "codex.session";
const CODEX_SOURCE_ROOT_LINEAGE_DOMAIN: &[u8] = b"ctx-codex-source-root-lineage-v1\0";
const CODEX_NATIVE_SESSION_NAMESPACE: &str = "codex.session";
const CODEX_LOGICAL_SESSION_KIND: &str = "codex-session";
const CODEX_LOGICAL_EVENT_KIND: &str = "codex-event";
const CODEX_SOURCE_SCHEMA_VARIANT: &str = "codex-nativepath-jsonl-v0";
const CODEX_PARSER_REVISION: &str = "codex-nativepath-core-activity-v8-inherited-session-lineage";

type CodexSessionPlanV0 = (CodexCatalogSource, SourceKey, String);

#[derive(Debug, Error)]
pub enum CodexSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Codex catalog discovery rejected {rejected} sources and failed {failed} sources")]
    IncompleteCatalog { rejected: usize, failed: usize },
    #[error("Codex catalog source {path:?} has no native session ID")]
    MissingNativeSessionId { path: PathBuf },
    #[error("Codex scanner emitted a row without lexical body text")]
    MissingLexicalBody,
    #[error("Codex source count overflow")]
    CountOverflow,
    #[error("Codex generation participant count overflow")]
    GenerationParticipantCountOverflow,
    #[error("Codex generation coordinator is unavailable")]
    GenerationCoordinatorUnavailable,
    #[error("explicit Codex session source changed its native session identity")]
    ExplicitSourceIdentityChanged,
}

pub type CodexSourceBackedResultV0<T> = Result<T, CodexSourceBackedErrorV0>;

impl From<CodexSourceBackedErrorV0> for CaptureError {
    fn from(error: CodexSourceBackedErrorV0) -> Self {
        match error {
            CodexSourceBackedErrorV0::Capture(error) => error,
            CodexSourceBackedErrorV0::Io(error) => Self::Io(error),
            CodexSourceBackedErrorV0::Json(error) => Self::Json(error),
            error => Self::InvalidPayload(error.to_string()),
        }
    }
}

mod catalog;
pub mod generation;
mod identity;
pub mod jsonl_family;
#[cfg(any(test, feature = "test-support"))]
pub use catalog::install_after_codex_metadata_inventory_hook;
pub(crate) use catalog::observe_codex_explicit_session_source_backed_v0;
pub use catalog::{
    absolute_lexical_path, codex_session_root_rank, CodexExplicitSessionSourceBackedInputV0,
};
pub use generation::{CodexGenerationNormalizationCoordinatorV0, CodexGenerationRouteV0};
pub(in crate::codex::nativepath) use identity::{
    codex_core_record, codex_session_identity, codex_source_key_in_root, CodexEventIdentityStateV0,
};
pub use jsonl_family::CodexSessionJsonlFamilyAdapterV0;

fn codex_session_tree_source_root_lineage(
    session_root: &Path,
) -> CodexSourceBackedResultV0<[u8; 32]> {
    let provider_home = match session_root.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("sessions" | "archived_sessions") => session_root.parent().unwrap_or(session_root),
        _ => session_root,
    };
    let identity =
        ctx_history_source_io::provider_path_identity(provider_home).map_err(CaptureError::from)?;
    let mut digest = Sha256::new();
    digest.update(CODEX_SOURCE_ROOT_LINEAGE_DOMAIN);
    digest.update((identity.len() as u64).to_be_bytes());
    digest.update(identity.as_bytes());
    Ok(digest.finalize().into())
}

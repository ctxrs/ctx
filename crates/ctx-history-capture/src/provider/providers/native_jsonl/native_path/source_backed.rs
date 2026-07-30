use std::path::PathBuf;

use ctx_history_core::{ProjectionContractError, SourceResolverContractError};
use thiserror::Error;

use super::{
    reader::{DirectJsonlProjector, ProjectedLine},
    DirectJsonlCheckpoint, DirectJsonlEvent, DirectJsonlRejection, DirectJsonlSession,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};
use crate::CaptureError;

mod adapter;
mod hydration;
mod lifecycle;
pub(crate) mod registration;

use adapter::direct_jsonl_session_identity;
#[cfg(test)]
use adapter::{inventory_traversals, reset_inventory_traversals};
pub(crate) use adapter::{
    DirectJsonlDisposition, DirectJsonlInventoryLeaf, DirectJsonlSelectedLeaf,
    DirectJsonlSourceAdapter, DirectJsonlSourceInventory,
};
pub(crate) use hydration::DirectJsonlHydrationCatalog;
use hydration::{hydrate_batch, hydrate_single};
#[cfg(test)]
use hydration::{hydration_work, reset_hydration_work, DirectJsonlHydrationWork};
#[cfg(test)]
use lifecycle::DirectJsonlScanReceipt;
pub(crate) use lifecycle::DirectJsonlSourceReader;
use lifecycle::{decode_certificate, decode_previous, DirectJsonlTerminalEvidenceSet};

const DIRECT_JSONL_SOURCE_IDENTITY_VERSION: u32 = 1;
const DIRECT_JSONL_SOURCE_BACKED_PARSER_REVISION: &str = "direct-native-jsonl-source-backed-v2";
const DIRECT_JSONL_SOURCE_FRONTIER_KIND: &str = "direct-native-jsonl-checkpoint-v1";
const DIRECT_JSONL_INVENTORY_AUTHORITY_NAMESPACE: &str = "direct-native-jsonl-provider-root-v2";
const DIRECT_JSONL_INVENTORY_REVISION_KIND: &str = "direct-native-jsonl-inventory-sha256-v2";
const DIRECT_JSONL_DISCOVERY_REVISION: &str = "direct-native-jsonl-discovery-v2";
const DIRECT_JSONL_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;
const DIRECT_JSONL_MAX_TOUCHED_FILES: usize = 256;
const DIRECT_JSONL_MAX_EXPANDED_RECORD_UNITS: usize = 64;
const DIRECT_JSONL_MAX_EXPANDED_RECORD_BYTES: usize = 8 * 1024 * 1024;
pub(super) const DIRECT_JSONL_MAX_REJECTION_DETAILS: usize = 64;

#[derive(Debug, Error)]
pub(crate) enum DirectJsonlSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("direct JSONL inventory is incomplete and cannot certify deletion")]
    IncompleteInventory,
    #[error("direct JSONL leaf {0:?} has no provider-native session identity")]
    MissingNativeSession(PathBuf),
    #[error("direct JSONL leaf changed provider-native session identity while scanning")]
    NativeSessionChanged,
    #[error(
        "direct JSONL leaf {path:?} rejected {count} records",
        count = .rejections.len()
    )]
    RejectedSource {
        path: PathBuf,
        rejections: Vec<DirectJsonlRejection>,
    },
    #[error("direct JSONL leaf scan did not reach a certified frontier")]
    IncompleteScan,
    #[error("direct JSONL scan counters do not reconcile")]
    CountMismatch,
    #[error("direct JSONL event has no exact source-record evidence")]
    MissingRecordEvidence,
    #[error("direct JSONL locator does not belong to this adapter and certified leaf")]
    InvalidLocator,
    #[error("the exact direct JSONL source is absent from the complete inventory")]
    SourceAbsent,
    #[error("direct JSONL locator range exceeds the bounded provider record size")]
    LocatorRangeTooLarge,
    #[error("direct JSONL locator no longer selects a retained lexical event")]
    LocatorRecordNotRetained,
    #[error("direct JSONL publication failed: {0}")]
    Publication(String),
}

pub(crate) type DirectJsonlSourceBackedResult<T> = Result<T, DirectJsonlSourceBackedError>;

#[cfg(test)]
#[path = "source_backed_test_support.rs"]
mod test_support;
#[cfg(test)]
pub(super) use test_support::assert_source_backed_fixture;

#[cfg(test)]
#[path = "source_backed_architecture_tests.rs"]
mod architecture_tests;

#[cfg(all(test, unix))]
#[path = "source_backed_lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(all(test, unix))]
#[path = "source_backed_authority_swap_tests.rs"]
mod authority_swap_tests;

//! Focused lifecycle and publication tests.
use super::StatusRequest;

#[path = "tests/active_merge.rs"]
mod active_merge;
#[path = "tests/direct_session.rs"]
mod direct_session;
#[cfg(unix)]
#[path = "tests/native_shell.rs"]
mod native_shell;
#[path = "tests/optional_root.rs"]
mod optional_root;
use std::collections::BTreeMap;
use std::error::Error;
use std::io;

use crate::core_materialization::CORE_MATERIALIZER_REVISION;
use crate::core_materialization::{
    CoreProjectionCoverage, PreparedCoreEventDeltaPage, PreparedCoreEvidence, PreparedCoreUnit,
};
use crate::envelope::{Confidence, Fact, FactState, ResourceRef};
use crate::graph::segment_graph::{
    SEGMENT_EVIDENCE_IDENTITY, SEGMENT_ORDERING_IDENTITY, SEGMENT_SCHEMA_IDENTITY,
};
use crate::protocol::{
    AgentScope, CoreEventDelta, CoreEventDeltaPage, CoreGenerationHead, CoreProjectionCurrentness,
    CoreRecord, CoreSourceDelta, CoreSourceDeltaPage, CoreSourceReconciliation, CoreSourceState,
    EvidenceCitation, IDENTITY_VERSION, ProviderNativeSessionRelationship, ResourceKind,
    SessionRelationshipKind, SourceAnchor, SourceKey, StableEntityId, StableEntityKind, TypedKey,
    core_record_contract_fingerprint,
};

use super::{SegmentMaterializer, SegmentMaterializerError};
use crate::graph::segment::{
    EventIndexSource, FILE_TOUCHED, IndexedCoreEventLineage, IndexedCoreEventOriginKind,
    IndexedCoreEventState,
};

pub(super) type TestResult<T = ()> = Result<T, Box<dyn Error>>;
include!("tests/support.rs");
include!("tests/batching.rs");

#[path = "tests/codex_policy_revision.rs"]
mod codex_policy_revision;

#[cfg(unix)]
#[path = "tests/provider_attribution.rs"]
mod provider_attribution;
#[path = "tests/provider_policy_revision.rs"]
mod provider_policy_revision;

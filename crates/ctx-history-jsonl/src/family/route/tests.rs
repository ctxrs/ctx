use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use super::super::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_jsonl_prefix_hash_hook,
    track_jsonl_prefix_hash_bytes, JsonlReader as RuntimeJsonlReader, JsonlRecordRef,
};
use super::*;
use crate::family::JsonlRecordRejections;
use ctx_history_capture_model::AttemptHistoryProgress;
use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
    CaptureLifecycleSink, CapturePublicationContext, CapturePublicationDisposition,
    CaptureRevalidationTarget, CaptureRouteRef, CaptureSourceAggregateRef, CoreMaterialization,
    CorePreparationFailureKind, CorePreparationPort, ImmutableCaptureSnapshot, PresentCaptureRoute,
    SourceBackedCertifiedRemoval, SourceBackedGenerationSink as RuntimeSourceBackedGenerationSink,
    SourceBackedLogicalSourceFailures, SourceBackedReconciliationDemand,
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDrafts,
    SourceBackedRecordRejections, SourceBackedRevalidationTarget, SourceBackedRouteResources,
    SourceOwner, VerifiedCapture,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSourceAppend, CertifiedSourceDeletion, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor,
};
use ctx_history_source_io::{SourceIoError, MAX_PROVIDER_JSONL_LINE_BYTES};

#[path = "tests/behavior.rs"]
mod behavior;
#[path = "tests/checkpoint_lifecycle.rs"]
mod checkpoint_lifecycle;

const TEST_SOURCE_FORMAT: &str = "terminal_witness_jsonl";
const TEST_SCHEMA: &str = "terminal-witness-v1";

fn test_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("00".repeat(32)).unwrap()
}

fn sibling_route_identity() -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap()
}

fn test_contract_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

type CaptureError = SourceIoError;
type Result<T> = std::result::Result<T, CaptureError>;
type OpenedProviderSourceFile = super::super::OpenedProviderSourceFile<CaptureError>;
type ProviderSourceRoot = super::super::ProviderSourceRoot<CaptureError>;
type JsonlReader = RuntimeJsonlReader<CaptureError>;
type JsonlFamilyLeaf = super::JsonlFamilyLeaf<CaptureError>;
type JsonlFamilyInventory = super::JsonlFamilyInventory<CaptureError>;
type JsonlFamilyMembershipObservation = super::JsonlFamilyMembershipObservation<CaptureError>;
type JsonlFamilyTerminalProof = super::JsonlFamilyTerminalProof<CaptureError>;
type JsonlFamilyOptimizedLeafOutcome = super::JsonlFamilyOptimizedLeafOutcome<CaptureError>;
type JsonlFamilyWorkerContext = super::JsonlFamilyWorkerContext<TestJsonlRuntime>;
type JsonlFamilyExecutionIo = super::JsonlFamilyExecutionIo<TestJsonlRuntime>;
type JsonlFamilyAdapterObject = dyn JsonlFamilyAdapter<Runtime = TestJsonlRuntime>;
type JsonlFamilyProjectorObject = dyn JsonlFamilyProjector<Runtime = TestJsonlRuntime>;
type JsonlFamilySemanticExecutorObject =
    dyn JsonlFamilySemanticExecutor<Runtime = TestJsonlRuntime>;
type FamilyResident = super::FamilyResident<CaptureError>;
type TerminalSourceEvidence = super::TerminalSourceEvidence<CaptureError>;
type JsonlFamilyAbsentMember = super::JsonlFamilyAbsentMember<CaptureError>;
type IndexBaseEventLookup = TestBaseEventLookup;
type IndexCaptureLifecycle = TestLifecycle;
type SourceBackedGenerationSink<'writer> =
    RuntimeSourceBackedGenerationSink<'writer, TestLifecycle>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct JsonlFamilyAdmissionActivity {
    selected_leaves: usize,
    bases: usize,
    retained_terminal_sources: usize,
    checkpoint_rejections: usize,
}

thread_local! {
    static JSONL_FAMILY_ADMISSION_ACTIVITY: std::cell::Cell<JsonlFamilyAdmissionActivity> =
        const { std::cell::Cell::new(JsonlFamilyAdmissionActivity {
            selected_leaves: 0,
            bases: 0,
            retained_terminal_sources: 0,
            checkpoint_rejections: 0,
        }) };
}

fn jsonl_family_admission_activity() -> JsonlFamilyAdmissionActivity {
    JSONL_FAMILY_ADMISSION_ACTIVITY.get()
}

pub(super) fn begin_admission(selected_leaves: usize, bases: usize) {
    JSONL_FAMILY_ADMISSION_ACTIVITY.set(JsonlFamilyAdmissionActivity {
        selected_leaves,
        bases,
        ..JsonlFamilyAdmissionActivity::default()
    });
}

pub(super) fn record_checkpoint_rejection() {
    let mut activity = JSONL_FAMILY_ADMISSION_ACTIVITY.get();
    activity.checkpoint_rejections += 1;
    JSONL_FAMILY_ADMISSION_ACTIVITY.set(activity);
}

pub(super) fn record_retained_sources(retained_terminal_sources: usize) {
    let mut activity = JSONL_FAMILY_ADMISSION_ACTIVITY.get();
    activity.retained_terminal_sources = retained_terminal_sources;
    JSONL_FAMILY_ADMISSION_ACTIVITY.set(activity);
}

#[path = "tests/shared_runtime.rs"]
mod shared_runtime;
use shared_runtime::*;
#[path = "tests/source_adapters.rs"]
mod source_adapters;
use source_adapters::*;
#[path = "tests/scheduler_support.rs"]
mod scheduler_support;
use scheduler_support::*;
#[path = "tests/semantic_support.rs"]
mod semantic_support;
use semantic_support::*;
#[path = "tests/projection_support.rs"]
mod projection_support;
use projection_support::*;
#[path = "tests/harness.rs"]
mod harness;
use harness::*;

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use sha2::{Digest, Sha256};

use ctx_history_capture_runtime::{CaptureLifecycleSink, SourceBackedRouteDriver};
pub use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_source_io::{
    MappedOpenedProviderSourceFile, MappedOpenedProviderSourcePath, MappedProviderSourceDirectory,
    MappedProviderSourceRoot, SourceIoError, MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod checkpoint;
mod framing;
mod identity;
mod physical;
mod rejections;
mod revalidation;
mod route;
mod single_file;

#[allow(
    unused_imports,
    reason = "shared family modules consume this compatibility prelude"
)]
pub(crate) use crate::{
    fit_jsonl_activity, jsonl_prefix_digest as prefix_digest, jsonl_terminal_call_id_digest,
    new_jsonl_prefix_hasher as new_prefix_hasher, ordered_pending_exchange_entries,
    remember_pending_exchange, restore_hash_pending_exchange_entries,
    restore_ordered_pending_exchange_entries, selected_content_fits as jsonl_selected_content_fits,
    sorted_pending_exchange_entries, take_pending_exchange, JsonlActivityObservedBytes,
    JsonlAppendOccurrenceState, JsonlCheckpoint, JsonlCheckpointedTerminalAuthority,
    JsonlFileObservation, JsonlOrderedAppendOccurrenceState, JsonlOversizedRecordPolicy, JsonlPage,
    JsonlPendingExchangeLookup, JsonlPendingExchangeRemember, JsonlPendingExchangeState,
    JsonlRecordEvidence, JsonlRecordRef, JsonlResumableSha256, JsonlScanOutcome, JsonlSourceChange,
    JsonlSourceIdentity, JsonlTerminalAuthority, JsonlTerminalObservationRegion,
};
pub use checkpoint::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
};

pub trait JsonlFamilyError:
    std::error::Error
    + From<std::io::Error>
    + From<serde_json::Error>
    + From<ctx_history_source_io::SourceIoError>
    + Send
    + Sync
    + 'static
{
    fn invalid_payload(detail: String) -> Self;
    fn system_invariant(detail: &'static str) -> Self;
    fn worker_panicked(worker: &'static str) -> Self;
    fn source_changed() -> Self;
    fn is_not_found(&self) -> bool;
    fn is_source_changed(&self) -> bool;
    fn is_resource_unavailable(&self) -> bool;
    fn is_internal(&self) -> bool;
    fn is_ignorable_membership_entry(&self) -> bool;
}

/// Static, provider-neutral configuration for one JSONL family integration.
///
/// Concrete storage, repository attribution, and route-control policy remain
/// in the integrating capture crate. The family monomorphizes over those
/// ports without error boxing or dynamic lifecycle dispatch.
pub trait JsonlFamilyRuntime: Send + Sync + 'static {
    type Error: JsonlFamilyError;
    type Lifecycle: CaptureLifecycleSink;
    type WorkerServices: Default + Send;
    type RouteControl: Clone + Send + Sync + 'static;

    fn begin_worker_leaf(services: &mut Self::WorkerServices);
}

pub type JsonlRuntimeError<R> = <R as JsonlFamilyRuntime>::Error;
pub type JsonlRuntimeLookup<R> =
    <<R as JsonlFamilyRuntime>::Lifecycle as CaptureLifecycleSink>::BaseLookup;
pub type JsonlRuntimeLifecycleError<R> =
    <<R as JsonlFamilyRuntime>::Lifecycle as CaptureLifecycleSink>::Error;
pub type JsonlRuntimeDriver<R> = SourceBackedRouteDriver<
    <R as JsonlFamilyRuntime>::Lifecycle,
    <R as JsonlFamilyRuntime>::RouteControl,
>;

impl JsonlFamilyError for SourceIoError {
    fn invalid_payload(detail: String) -> Self {
        Self::InvalidPayload(detail)
    }

    fn system_invariant(detail: &'static str) -> Self {
        Self::SystemInvariant(detail)
    }

    fn worker_panicked(worker: &'static str) -> Self {
        Self::SystemInvariant(worker)
    }

    fn source_changed() -> Self {
        Self::SourceChangedDuringCapture
    }

    fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
            || matches!(self, Self::SystemIo { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }

    fn is_source_changed(&self) -> bool {
        matches!(self, Self::SourceChangedDuringCapture)
            || matches!(
                self,
                Self::InvalidProviderTranscriptPath { reason, .. }
                    if *reason == "provider source changed while its authority handle was retained"
            )
    }

    fn is_resource_unavailable(&self) -> bool {
        matches!(self, Self::Io(_) | Self::SystemIo { .. }) && !self.is_not_found()
    }

    fn is_internal(&self) -> bool {
        matches!(self, Self::SystemInvariant(_))
    }

    fn is_ignorable_membership_entry(&self) -> bool {
        ctx_history_source_io::is_symlink_source_rejection(self)
            || ctx_history_source_io::is_non_regular_source_rejection(self)
    }
}

pub type JsonlResult<T, E> = std::result::Result<T, E>;
pub type OpenedProviderSourceFile<E> = MappedOpenedProviderSourceFile<E>;
pub type OpenedProviderSourcePath<E> = MappedOpenedProviderSourcePath<E>;
pub type ProviderSourceDirectory<E> = MappedProviderSourceDirectory<E>;
pub type ProviderSourceRoot<E> = MappedProviderSourceRoot<E>;

#[cfg(test)]
type CaptureError = SourceIoError;
#[cfg(test)]
type Result<T> = std::result::Result<T, CaptureError>;
use framing::read_bounded_record_complete_sha256;
pub use framing::{
    read_bounded_record, read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_full_complete_and_prefix_sha256, read_bounded_record_unhashed,
    JsonlRecordFraming,
};
use identity::observe_metadata;
#[cfg(any(test, feature = "test-support"))]
pub use physical::set_after_standard_zstd_snapshot_hook;
pub use physical::{
    JsonlPhysicalDigest, JsonlPhysicalEncoding, JsonlPhysicalRecord, JsonlPhysicalStream,
    JsonlPhysicalStreamPosition, MAX_STANDARD_ZSTD_COMPRESSED_BYTES,
    MAX_STANDARD_ZSTD_DECOMPRESSED_BYTES, MAX_STANDARD_ZSTD_PARALLEL_STREAMS,
    MAX_STANDARD_ZSTD_TEMP_BYTES_PER_LEAF,
};
pub use rejections::JsonlRecordRejections;
use revalidation::hash_prefix;
pub use revalidation::revalidate_frozen_prefix;
pub(crate) use revalidation::{
    authenticate_frozen_prefix, authenticate_frozen_prefix_sha256, revalidate_frozen_prefix_sha256,
};
#[cfg(any(test, feature = "test-support"))]
pub use revalidation::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_final_jsonl_prefix_hash_hook,
    set_after_jsonl_append_observation_route_binding_hook, set_after_jsonl_prefix_hash_hook,
    set_after_jsonl_semantic_preflight_hook, set_after_second_jsonl_prefix_hash_hook,
    track_jsonl_prefix_hash_bytes, JsonlPrefixHashBytesGuard,
};
pub use revalidation::{observe_opened_file, observe_opened_file_allow_append};
#[cfg(any(test, feature = "test-support"))]
pub use route::{
    checkpoint_admitted_revision_for_test, set_before_jsonl_terminal_physical_revalidation_hook,
};
pub use route::{
    jsonl_family_driver, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyAppendTrustContract,
    JsonlFamilyExecutionIo, JsonlFamilyExecutionPosition, JsonlFamilyInventory,
    JsonlFamilyInventoryMember, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
    JsonlFamilyLeafDisposition, JsonlFamilyMembershipObservation, JsonlFamilyOpenedMember,
    JsonlFamilyOptimizedLeafOutcome, JsonlFamilyPendingLeaf, JsonlFamilyPhysicalSourceIdentity,
    JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyProjectorPreflightError,
    JsonlFamilyPublication, JsonlFamilyRejectedLeaf, JsonlFamilyRootMissingMode,
    JsonlFamilySemanticExecutor, JsonlFamilySemanticPage, JsonlFamilySemanticPreflight,
    JsonlFamilySemanticSummary, JsonlFamilyTerminalProof, JsonlFamilyWorkerContext,
    JSONL_FAMILY_MAX_LEAF_TERMINAL_DEPENDENCIES, JSONL_FAMILY_MAX_LEAF_TERMINAL_PRESENT_BYTES,
};
pub use single_file::jsonl_single_file_inventory;
const PAGE_MAX_RECORDS: usize = 64;
pub const JSONL_FAMILY_SEMANTIC_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const PAGE_MAX_BYTES: usize = JSONL_FAMILY_SEMANTIC_PAGE_MAX_BYTES;

#[derive(Debug, Clone)]
pub struct JsonlProbe {
    observation: JsonlFileObservation,
    prefix_hasher: JsonlResumableSha256,
    complete_prefix_end: u64,
    next_physical_ordinal: u64,
}

impl JsonlProbe {
    pub fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }
}

pub struct JsonlReader<E: JsonlFamilyError> {
    identity: JsonlSourceIdentity,
    observation: JsonlFileObservation,
    source_file: Arc<OpenedProviderSourceFile<E>>,
    reader: Option<BufReader<File>>,
    physical: Option<JsonlPhysicalStream<E>>,
    prefix_hasher: JsonlResumableSha256,
    complete_prefix_end: u64,
    next_physical_ordinal: u64,
    source_change: JsonlSourceChange,
    skip_scan: bool,
    unchanged_checkpoint: Option<JsonlCheckpoint>,
    finished: bool,
    outcome: Option<JsonlScanOutcome>,
    record_buffer: Vec<u8>,
    whole_record: bool,
    append_log: bool,
    bind_admitted_eof: bool,
    logical_eof: Option<u64>,
    complete_prefix_ends_with_terminal_nul_padding: bool,
    semantic_append_resume: Option<JsonlSemanticAppendResume>,
    direct_append_resume: bool,
    semantic_preflight_binding: Option<JsonlSemanticPreflightBinding>,
    oversized_record_policy: JsonlOversizedRecordPolicy,
}

struct JsonlSemanticAppendResume {
    previous: JsonlCheckpoint,
    admitted_eof_sha256: Option<[u8; 32]>,
    position: Option<JsonlPhysicalStreamPosition>,
}

struct JsonlReaderFramingOptions<'a> {
    physical_encoding: JsonlPhysicalEncoding,
    record_framing: JsonlRecordFraming,
    whole_record: bool,
    bind_admitted_eof: bool,
    logical_eof: Option<u64>,
    deferred_append_eof_sha256: Option<Option<[u8; 32]>>,
    frozen_observation: Option<&'a JsonlFileObservation>,
    direct_append: bool,
    route_resources: Option<&'a ctx_history_capture_runtime::SourceBackedRouteResources>,
}

pub enum JsonlSemanticPreflightMode {
    AdmittedEof(Option<[u8; 32]>),
    CompletePrefix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonlSemanticPreflightBinding {
    physical: physical::JsonlPhysicalPassBinding,
    complete_prefix_ends_with_terminal_nul_padding: bool,
}

mod reader;
/// Projects the first complete physical record and returns its prefix state.
///
/// Cold and replacement scans resume after this record, so the provider parser
/// sees every physical record at most once. Append and unchanged scans discard
/// the probe state after binding identity.
pub fn probe_first_record<T, E, V>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile<E>>,
    visit: impl FnOnce(JsonlRecordRef<'_>) -> JsonlResult<T, V>,
) -> JsonlResult<(T, JsonlProbe), V>
where
    E: JsonlFamilyError,
    V: From<E>,
{
    let mut visit = Some(visit);
    probe_records_until(source_path, source_file, 1, |record| {
        visit.take().ok_or_else(|| {
            V::from(E::system_invariant(
                "provider identity probe visited more than one record",
            ))
        })?(record)
        .map(Some)
    })?
    .ok_or_else(|| {
        V::from(E::invalid_payload(
            "provider identity record is missing or incomplete".to_owned(),
        ))
    })
}

pub fn probe_records_until<T, E, V>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile<E>>,
    max_records: usize,
    mut visit: impl FnMut(JsonlRecordRef<'_>) -> JsonlResult<Option<T>, V>,
) -> JsonlResult<Option<(T, JsonlProbe)>, V>
where
    E: JsonlFamilyError,
    V: From<E>,
{
    if max_records == 0 || max_records > PAGE_MAX_RECORDS {
        return Err(V::from(E::system_invariant(
            "provider identity probe record bound is invalid",
        )));
    }
    source_file.revalidate_same_object().map_err(V::from)?;
    let observation = observe_metadata::<E>(
        source_path,
        source_file.file(),
        &source_file.file().metadata().map_err(E::from)?,
    )
    .map_err(V::from)?;
    let mut file = source_file.reopen_same_object().map_err(V::from)?;
    file.seek(SeekFrom::Start(0))
        .map_err(E::from)
        .map_err(V::from)?;
    let mut reader = BufReader::new(file);
    let mut hasher = new_prefix_hasher();
    let mut buffer = Vec::new();
    let mut start = 0_u64;
    for ordinal in 0..max_records {
        let (end, record_digest, _wire_bytes) = match read_bounded_line::<E>(
            &mut reader,
            &mut buffer,
            &mut hasher,
            observation.length(),
            start,
        )
        .map_err(V::from)?
        {
            RawLine::Complete {
                end,
                record_digest,
                wire_bytes,
            } => (end, record_digest, wire_bytes),
            RawLine::EndOfFile | RawLine::IncompleteTail => break,
            RawLine::Oversized => {
                return Err(V::from(E::invalid_payload(format!(
                    "provider identity record exceeds the {} byte JSONL record limit",
                    MAX_PROVIDER_JSONL_LINE_BYTES
                ))));
            }
        };
        let physical_ordinal = u64::try_from(ordinal).map_err(|_| {
            V::from(E::system_invariant(
                "provider identity probe ordinal exceeds u64",
            ))
        })?;
        if let Some(value) = visit(JsonlRecordRef::new(
            &buffer,
            JsonlRecordEvidence::new(physical_ordinal, start, end, record_digest),
            false,
        ))? {
            let closing = revalidate_frozen_prefix(
                source_path,
                source_file.as_ref(),
                &observation,
                end,
                prefix_digest(&hasher),
            )
            .map_err(V::from)?;
            return Ok(Some((
                value,
                JsonlProbe {
                    observation: closing,
                    prefix_hasher: hasher,
                    complete_prefix_end: end,
                    next_physical_ordinal: physical_ordinal.saturating_add(1),
                },
            )));
        }
        start = end;
    }
    revalidate_frozen_prefix(
        source_path,
        source_file.as_ref(),
        &observation,
        start,
        prefix_digest(&hasher),
    )
    .map_err(V::from)?;
    Ok(None)
}

enum RawLine {
    EndOfFile,
    IncompleteTail,
    Oversized,
    Complete {
        end: u64,
        record_digest: [u8; 32],
        wire_bytes: usize,
    },
}

fn read_bounded_line<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    bytes: &mut Vec<u8>,
    hasher: &mut JsonlResumableSha256,
    frozen_length: u64,
    start: u64,
) -> JsonlResult<RawLine, E> {
    bytes.clear();
    if start >= frozen_length {
        return Ok(RawLine::EndOfFile);
    }
    let Some(record) = read_bounded_record_complete_sha256(
        reader,
        bytes,
        hasher,
        frozen_length.saturating_sub(start),
        JsonlRecordFraming::ordinary(),
        E::source_changed,
    )?
    else {
        return Ok(RawLine::EndOfFile);
    };
    if !record.complete {
        return Ok(RawLine::IncompleteTail);
    }
    let end = start
        .checked_add(record.byte_len)
        .ok_or_else(|| E::system_invariant("JSONL byte offset overflowed"))?;
    let wire_bytes = usize::try_from(record.byte_len).unwrap_or(usize::MAX);
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if record.oversized || bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
        bytes.clear();
        return Ok(RawLine::Oversized);
    }
    Ok(RawLine::Complete {
        end,
        record_digest: record.sha256,
        wire_bytes,
    })
}

#[cfg(test)]
mod tests;

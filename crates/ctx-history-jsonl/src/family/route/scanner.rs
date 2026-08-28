use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CoreRecord, SourceKey, SourceObservation, TypedKey,
};

use super::super::{
    JsonlFamilyError, JsonlFamilyRuntime, JsonlFileObservation, JsonlPhysicalRecord,
    JsonlPhysicalStreamPosition, JsonlReader, JsonlResult, JsonlRuntimeError,
    JsonlRuntimeLifecycleError, JsonlSourceIdentity,
};
use super::{
    contract_error, route_internal, JsonlFamilyAdapter, JsonlFamilyLeaf, JsonlFamilyTerminalProof,
    FAMILY_POLICY_REVISION, FAMILY_SOURCE_REVISION_KIND,
};
use crate::family::{PAGE_MAX_BYTES, PAGE_MAX_RECORDS};
use ctx_history_capture_runtime::{
    ParallelLeafScanEmitError, ParallelLeafScanError, SourceBackedCoordinatorError,
    SourceBackedRecordRejectionDrafts, SourceBackedRouteError,
};

pub(super) fn preserve_coordinator_error<R: JsonlFamilyRuntime>(
    failure: &mut Option<SourceBackedRouteError>,
    error: SourceBackedCoordinatorError<JsonlRuntimeLifecycleError<R>>,
) -> JsonlRuntimeError<R> {
    let error = match error {
        SourceBackedCoordinatorError::CoreEmission(source) => source,
        error => route_internal(error),
    };
    preserve_route_error(failure, error)
}

fn preserve_route_error<E: JsonlFamilyError>(
    failure: &mut Option<SourceBackedRouteError>,
    error: SourceBackedRouteError,
) -> E {
    let detail = error.to_string();
    *failure = Some(error);
    E::invalid_payload(detail)
}

pub(super) fn preserve_parallel_emit_error<E: JsonlFamilyError>(
    failure: &mut Option<SourceBackedRouteError>,
    error: ParallelLeafScanEmitError,
) -> E {
    match error {
        ParallelLeafScanEmitError::Route(error) => preserve_route_error(failure, error),
        ParallelLeafScanEmitError::Cancelled(_) => {
            E::system_invariant("JSONL parallel scan was cancelled during replacement")
        }
    }
}

pub(super) fn map_parallel_leaf_error<E: std::error::Error + 'static>(
    error: ParallelLeafScanError<SourceBackedRouteError, E>,
) -> SourceBackedRouteError {
    match error {
        ParallelLeafScanError::Worker { source, .. } => source,
        ParallelLeafScanError::Sink { source, .. } => match *source {
            SourceBackedCoordinatorError::CoreEmission(source) => source,
            source => route_internal(source),
        },
        other => route_internal(other),
    }
}

pub(super) fn physical_identity<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
) -> JsonlSourceIdentity {
    let encoding = adapter.physical_encoding(leaf);
    let policy_revision = match encoding {
        super::super::JsonlPhysicalEncoding::RawJsonl => FAMILY_POLICY_REVISION.to_owned(),
        _ => format!("{FAMILY_POLICY_REVISION}:{}", encoding.checkpoint_tag()),
    };
    JsonlSourceIdentity::new(
        adapter.provider().as_str(),
        adapter.parser_revision(),
        policy_revision,
        leaf.source.exact_descriptor_digest(),
        leaf.source_path.clone(),
    )
}

pub(super) fn source_observation<E: JsonlFamilyError>(
    source: &SourceKey,
    observation: &JsonlFileObservation,
) -> JsonlResult<SourceObservation, E> {
    SourceObservation::new(
        source.clone(),
        FAMILY_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )
    .map_err(contract_error)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyAppendMode {
    CertifiedSuffix,
    Replacement,
    ProjectorPreflight(bool),
}

impl JsonlFamilyAppendMode {
    pub(super) fn certified_suffix(self) -> bool {
        matches!(self, Self::CertifiedSuffix | Self::ProjectorPreflight(true))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyProjectionMode {
    Cold,
    CertifiedAppend,
    Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilySemanticPreflight {
    Ready,
    RetryReplacement,
}

/// Legacy optimized publication mode retained for non-Codex adapters while
/// they migrate independently to the shared executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyPublication {
    Append,
    Replace,
}

/// Mutable services reused by one shared JSONL scanner worker across every
/// leaf in its stripe. Keeping these caches at worker lifetime preserves
/// bounded parallelism while amortizing provider-neutral projection work.
pub struct JsonlFamilyWorkerContext<R: JsonlFamilyRuntime> {
    services: R::WorkerServices,
}

impl<R: JsonlFamilyRuntime> Default for JsonlFamilyWorkerContext<R> {
    fn default() -> Self {
        Self {
            services: R::WorkerServices::default(),
        }
    }
}

impl<R: JsonlFamilyRuntime> JsonlFamilyWorkerContext<R> {
    pub(super) fn begin_leaf(&mut self) {
        R::begin_worker_leaf(&mut self.services);
    }

    pub fn services(&mut self) -> &mut R::WorkerServices {
        &mut self.services
    }
}

/// Legacy optimized outcome retained for non-Codex adapters. Codex uses the
/// semantic executor, which cannot construct lifecycle evidence.
#[derive(Debug)]
pub struct JsonlFamilyOptimizedLeafOutcome<E: super::super::JsonlFamilyError> {
    pub(super) certificate: CertifiedSource,
    pub(super) append: Option<CertifiedSourceAppend>,
    pub(super) terminal_proof: JsonlFamilyTerminalProof<E>,
}

impl<E: super::super::JsonlFamilyError> Clone for JsonlFamilyOptimizedLeafOutcome<E> {
    fn clone(&self) -> Self {
        Self {
            certificate: self.certificate.clone(),
            append: self.append.clone(),
            terminal_proof: self.terminal_proof.clone(),
        }
    }
}

impl<E: super::super::JsonlFamilyError> JsonlFamilyOptimizedLeafOutcome<E> {
    pub fn replacement(
        certificate: CertifiedSource,
        terminal_proof: JsonlFamilyTerminalProof<E>,
    ) -> Self {
        Self {
            certificate,
            append: None,
            terminal_proof,
        }
    }

    pub fn append(
        append: CertifiedSourceAppend,
        terminal_proof: JsonlFamilyTerminalProof<E>,
    ) -> Self {
        Self {
            certificate: append.current().clone(),
            append: Some(append),
            terminal_proof,
        }
    }
}

#[derive(Debug)]
pub struct JsonlFamilySemanticPage {
    records: Vec<CoreRecord>,
    encoded_byte_limit: usize,
}

impl JsonlFamilySemanticPage {
    pub fn new(records: Vec<CoreRecord>) -> Self {
        Self {
            records,
            encoded_byte_limit: PAGE_MAX_BYTES,
        }
    }

    /// Splits projected records into publication pages without changing their
    /// order. A single record that exceeds the byte cap remains invalid: it
    /// cannot be represented by any bounded semantic page.
    pub fn split_bounded<E: JsonlFamilyError>(
        records: Vec<CoreRecord>,
    ) -> JsonlResult<Vec<Self>, E> {
        Self::split_bounded_with_singleton_limit(records, PAGE_MAX_BYTES)
    }

    /// Splits projected records at the ordinary page limit while allowing one
    /// provider-bounded record to occupy a larger singleton page.
    pub fn split_bounded_with_singleton_limit<E: JsonlFamilyError>(
        records: Vec<CoreRecord>,
        singleton_byte_limit: usize,
    ) -> JsonlResult<Vec<Self>, E> {
        if singleton_byte_limit < PAGE_MAX_BYTES {
            return Err(E::invalid_payload(format!(
                "JSONL semantic singleton byte limit is below {PAGE_MAX_BYTES}"
            )));
        }
        let encoded_lengths = semantic_record_encoded_lengths::<E>(&records)?;
        let ranges = bounded_semantic_page_ranges_with_singleton_limit::<E>(
            &encoded_lengths,
            singleton_byte_limit,
        )?;
        let mut records = records.into_iter();
        let mut pages = Vec::with_capacity(ranges.len());
        for range in ranges {
            let encoded_bytes = checked_semantic_page_byte_total::<E>(
                encoded_lengths[range.clone()].iter().copied(),
            )?;
            let encoded_byte_limit = if range.len() == 1 && encoded_bytes > PAGE_MAX_BYTES {
                singleton_byte_limit
            } else {
                PAGE_MAX_BYTES
            };
            pages.push(Self {
                records: records.by_ref().take(range.len()).collect(),
                encoded_byte_limit,
            });
        }
        Ok(pages)
    }

    pub fn records(&self) -> &[CoreRecord] {
        &self.records
    }

    pub(super) fn into_bounded_records<E: JsonlFamilyError>(
        self,
    ) -> JsonlResult<Vec<CoreRecord>, E> {
        if self.records.len() > PAGE_MAX_RECORDS {
            return Err(E::invalid_payload(format!(
                "JSONL semantic page exceeds the {PAGE_MAX_RECORDS} record limit"
            )));
        }
        let encoded_bytes = checked_semantic_page_byte_total::<E>(
            semantic_record_encoded_lengths::<E>(&self.records)?,
        )?;
        if encoded_bytes > self.encoded_byte_limit {
            return Err(E::invalid_payload(format!(
                "JSONL semantic page exceeds the {} byte limit",
                self.encoded_byte_limit
            )));
        }
        Ok(self.records)
    }
}

fn semantic_record_encoded_lengths<E: JsonlFamilyError>(
    records: &[CoreRecord],
) -> JsonlResult<Vec<usize>, E> {
    records
        .iter()
        .map(|record| {
            record
                .encode_stored()
                .map(|encoded| encoded.len())
                .map_err(|error| E::invalid_payload(error.to_string()))
        })
        .collect()
}

#[cfg(test)]
fn bounded_semantic_page_ranges<E: JsonlFamilyError>(
    encoded_lengths: &[usize],
) -> JsonlResult<Vec<std::ops::Range<usize>>, E> {
    bounded_semantic_page_ranges_with_singleton_limit::<E>(encoded_lengths, PAGE_MAX_BYTES)
}

fn bounded_semantic_page_ranges_with_singleton_limit<E: JsonlFamilyError>(
    encoded_lengths: &[usize],
    singleton_byte_limit: usize,
) -> JsonlResult<Vec<std::ops::Range<usize>>, E> {
    let mut ranges = Vec::new();
    let mut page_start = 0_usize;
    let mut page_bytes = 0_usize;
    for (index, &encoded_length) in encoded_lengths.iter().enumerate() {
        if encoded_length > singleton_byte_limit {
            return Err(E::invalid_payload(format!(
                "JSONL semantic page exceeds the {singleton_byte_limit} byte limit"
            )));
        }
        if encoded_length > PAGE_MAX_BYTES {
            if page_start < index {
                ranges.push(page_start..index);
            }
            ranges.push(index..index + 1);
            page_start = index + 1;
            page_bytes = 0;
            continue;
        }
        let page_records = index.saturating_sub(page_start);
        let next_page_bytes = page_bytes.checked_add(encoded_length).ok_or_else(|| {
            E::invalid_payload("JSONL semantic page byte count overflowed".to_owned())
        })?;
        if page_records == PAGE_MAX_RECORDS || next_page_bytes > PAGE_MAX_BYTES {
            ranges.push(page_start..index);
            page_start = index;
            page_bytes = encoded_length;
        } else {
            page_bytes = next_page_bytes;
        }
    }
    if page_start < encoded_lengths.len() || encoded_lengths.is_empty() {
        ranges.push(page_start..encoded_lengths.len());
    }
    Ok(ranges)
}

fn checked_semantic_page_byte_total<E: JsonlFamilyError>(
    lengths: impl IntoIterator<Item = usize>,
) -> JsonlResult<usize, E> {
    lengths.into_iter().try_fold(0_usize, |total, length| {
        total.checked_add(length).ok_or_else(|| {
            E::invalid_payload("JSONL semantic page byte count overflowed".to_owned())
        })
    })
}

#[cfg(test)]
mod semantic_page_bound_tests {
    use super::*;
    use ctx_history_core::{
        derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
        SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    };
    use ctx_history_source_io::SourceIoError;

    #[test]
    fn semantic_page_byte_total_rejects_checked_sum_overflow() {
        let error = checked_semantic_page_byte_total::<SourceIoError>([usize::MAX, 1])
            .expect_err("semantic page byte sum overflow must fail closed");
        assert!(error.to_string().contains("byte count overflowed"));
    }

    #[test]
    fn semantic_page_ranges_split_record_and_exact_byte_bounds_in_order() {
        assert_eq!(
            bounded_semantic_page_ranges::<SourceIoError>(&[1; PAGE_MAX_RECORDS + 1]).unwrap(),
            [0..PAGE_MAX_RECORDS, PAGE_MAX_RECORDS..PAGE_MAX_RECORDS + 1]
        );
        assert_eq!(
            bounded_semantic_page_ranges::<SourceIoError>(&[
                PAGE_MAX_BYTES / 2 + 1,
                PAGE_MAX_BYTES / 2,
            ])
            .unwrap(),
            [0..1, 1..2]
        );
        assert!(bounded_semantic_page_ranges::<SourceIoError>(&[PAGE_MAX_BYTES + 1]).is_err());
        let empty = bounded_semantic_page_ranges::<SourceIoError>(&[]).unwrap();
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0], 0..0);
    }

    #[test]
    fn semantic_pages_allow_only_explicit_bounded_singletons_above_default_limit() {
        let source = SourceKey::derive(
            "codex",
            "codex_session_jsonl_tree",
            "session",
            1,
            SourceAnchor::CatalogLineage([1; 32]),
        )
        .unwrap();
        let native_session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "session",
            native_session_key: &native_session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let oversized = CoreRecord::new_selected(
            event_id,
            session_id,
            source,
            1,
            "message",
            "jsonl-semantic-page-test-v1",
            "x".repeat(PAGE_MAX_BYTES + 1),
        )
        .unwrap();

        assert!(
            JsonlFamilySemanticPage::split_bounded::<SourceIoError>(vec![oversized.clone()])
                .is_err()
        );
        let mut before = oversized.clone();
        before.event_sequence = 0;
        before.content.normalized_body = Some("before".to_owned());
        let mut after = before.clone();
        after.event_sequence = 2;
        after.content.normalized_body = Some("after".to_owned());
        let pages = JsonlFamilySemanticPage::split_bounded_with_singleton_limit::<SourceIoError>(
            vec![before, oversized, after],
            16 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(pages.len(), 3);
        let sequences = pages
            .into_iter()
            .map(|page| {
                let records = page.into_bounded_records::<SourceIoError>().unwrap();
                assert_eq!(records.len(), 1);
                records[0].event_sequence
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![0, 1, 2]);
    }
}

#[derive(Debug)]
pub struct JsonlFamilySemanticSummary {
    represented_physical_records: u64,
    rejected_records: u64,
    logical_complete_records: Option<u64>,
    rejected_logical_records: Option<u64>,
    provider_checkpoint: Option<TypedKey>,
    record_rejections: SourceBackedRecordRejectionDrafts,
    logical_source_quarantine: Option<(SourceKey, String)>,
}

impl JsonlFamilySemanticSummary {
    pub fn new(
        represented_physical_records: u64,
        rejected_records: u64,
        provider_checkpoint: Option<TypedKey>,
    ) -> Self {
        Self {
            represented_physical_records,
            rejected_records,
            logical_complete_records: None,
            rejected_logical_records: None,
            provider_checkpoint,
            record_rejections: SourceBackedRecordRejectionDrafts::default(),
            logical_source_quarantine: None,
        }
    }

    pub fn with_logical_counts(
        represented_physical_records: u64,
        rejected_records: u64,
        logical_complete_records: u64,
        rejected_logical_records: u64,
        provider_checkpoint: Option<TypedKey>,
    ) -> Self {
        Self {
            represented_physical_records,
            rejected_records,
            logical_complete_records: Some(logical_complete_records),
            rejected_logical_records: Some(rejected_logical_records),
            provider_checkpoint,
            record_rejections: SourceBackedRecordRejectionDrafts::default(),
            logical_source_quarantine: None,
        }
    }

    pub fn with_record_rejections(
        mut self,
        record_rejections: SourceBackedRecordRejectionDrafts,
    ) -> Self {
        self.record_rejections = record_rejections;
        self
    }

    /// Marks this scanned leaf as unpublishable without attributing it to the
    /// provider-native source key that ownership validation rejected.
    pub fn with_logical_source_quarantine(
        mut self,
        source: SourceKey,
        detail: impl Into<String>,
    ) -> Self {
        self.logical_source_quarantine = Some((source, detail.into()));
        self
    }

    pub(super) fn represented_physical_records(&self) -> u64 {
        self.represented_physical_records
    }

    pub(super) fn rejected_records(&self) -> u64 {
        self.rejected_records
    }

    pub(super) fn logical_source_quarantine(&self) -> Option<&(SourceKey, String)> {
        self.logical_source_quarantine.as_ref()
    }

    pub(super) fn logical_complete_records(&self) -> Option<u64> {
        self.logical_complete_records
    }

    pub(super) fn rejected_logical_records(&self) -> Option<u64> {
        self.rejected_logical_records
    }

    pub(super) fn into_record_rejections(self) -> SourceBackedRecordRejectionDrafts {
        self.record_rejections
    }

    pub(super) fn provider_checkpoint(&self) -> Option<TypedKey> {
        self.provider_checkpoint.clone()
    }
}

/// Shared-owned physical input for an adapter's bounded semantic executor.
///
/// The executor may consume and roll back records for provider-native paging,
/// but it cannot construct or return physical checkpoints, certificates,
/// append evidence, terminal proof, or publication mode. Those remain sealed
/// in the family driver.
pub struct JsonlFamilyExecutionIo<R: JsonlFamilyRuntime> {
    reader: JsonlReader<JsonlRuntimeError<R>>,
}

impl<R: JsonlFamilyRuntime> JsonlFamilyExecutionIo<R> {
    pub fn new(reader: JsonlReader<JsonlRuntimeError<R>>) -> Self {
        Self { reader }
    }

    pub fn next_record(
        &mut self,
    ) -> JsonlResult<Option<JsonlFamilyExecutionRecord>, JsonlRuntimeError<R>> {
        self.reader
            .next_execution_record()
            .map(|record| record.map(JsonlFamilyExecutionRecord::new))
    }

    pub fn record_bytes(
        &self,
        record: JsonlFamilyExecutionRecord,
    ) -> JsonlResult<&[u8], JsonlRuntimeError<R>> {
        self.reader.execution_record_bytes(record.physical)
    }

    pub fn position(&self) -> JsonlResult<JsonlFamilyExecutionPosition, JsonlRuntimeError<R>> {
        self.reader
            .execution_position()
            .map(|physical| JsonlFamilyExecutionPosition { physical })
    }

    pub fn settle_preflight(
        &mut self,
        initial: JsonlFamilyExecutionPosition,
    ) -> JsonlResult<bool, JsonlRuntimeError<R>> {
        self.reader
            .settle_semantic_preflight(initial.physical, true, false)
    }

    pub fn restore(
        &mut self,
        position: JsonlFamilyExecutionPosition,
    ) -> JsonlResult<(), JsonlRuntimeError<R>> {
        self.reader.restore_execution_position(position.physical)
    }

    pub fn offset(&self) -> JsonlResult<u64, JsonlRuntimeError<R>> {
        self.reader.execution_offset()
    }

    pub fn complete_prefix_end(&self) -> JsonlResult<u64, JsonlRuntimeError<R>> {
        self.reader.execution_complete_prefix_end()
    }

    /// Boundary between the previously certified source prefix and bytes
    /// admitted by this append. No physical checkpoint state crosses the
    /// shared executor seam.
    pub fn certified_prefix_end(&self) -> Option<u64> {
        self.reader.execution_certified_prefix_end()
    }

    /// Whether the physical reader resumed at the certified frontier and will
    /// expose only appended records to this executor.
    pub fn is_direct_append_resume(&self) -> bool {
        self.reader.execution_is_direct_append_resume()
    }

    pub fn release_record_buffer(&mut self) -> JsonlResult<(), JsonlRuntimeError<R>> {
        self.reader.release_execution_record_buffer()
    }

    pub(super) fn admitted_eof_sha256(
        &self,
    ) -> JsonlResult<Option<[u8; 32]>, JsonlRuntimeError<R>> {
        self.reader.admitted_eof_sha256()
    }

    pub(super) fn complete_prefix_ends_with_terminal_nul_padding(&self) -> bool {
        self.reader.complete_prefix_ends_with_terminal_nul_padding()
    }

    pub(super) fn into_reader(self) -> JsonlReader<JsonlRuntimeError<R>> {
        self.reader
    }
}

#[derive(Debug, Clone)]
pub struct JsonlFamilyExecutionPosition {
    physical: JsonlPhysicalStreamPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlFamilyExecutionRecord {
    physical: JsonlPhysicalRecord,
}

impl JsonlFamilyExecutionRecord {
    fn new(physical: JsonlPhysicalRecord) -> Self {
        Self { physical }
    }

    pub fn physical_ordinal(self) -> u64 {
        self.physical.physical_ordinal
    }

    pub fn byte_start(self) -> u64 {
        self.physical.byte_start
    }

    pub fn byte_end_exclusive(self) -> u64 {
        self.physical.byte_end_exclusive
    }

    pub fn byte_len(self) -> u64 {
        self.physical.byte_len()
    }

    pub fn complete(self) -> bool {
        self.physical.complete
    }

    pub fn terminal_nul_padding(self) -> bool {
        self.physical.terminal_nul_padding
    }

    pub fn oversized(self) -> bool {
        self.physical.oversized
    }

    pub fn stored_len(self) -> usize {
        self.physical.stored_len
    }

    pub fn sha256(self) -> [u8; 32] {
        self.physical.sha256
    }
}

#[cfg(test)]
pub(crate) use activity::with_family_scanner_workers;
#[cfg(test)]
pub(super) use activity::{
    jsonl_family_scanner_activity, jsonl_family_scanner_probe,
    record_jsonl_family_scanner_activity, JsonlFamilyScannerActivity, JsonlFamilyScannerProbe,
    FAMILY_SCANNER_WORKERS_OVERRIDE,
};

#[cfg(test)]
mod activity {
    use std::{
        cell::Cell,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        },
    };

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct JsonlFamilyScannerActivity {
        pub(crate) worker_count: usize,
        pub(crate) sources_started: usize,
        pub(crate) sources_completed: usize,
        pub(crate) peak_active_scanners: usize,
    }

    thread_local! {
        pub(in super::super) static FAMILY_SCANNER_WORKERS_OVERRIDE: Cell<Option<usize>> =
            const { Cell::new(None) };
        static FAMILY_SCANNER_ACTIVITY: Cell<JsonlFamilyScannerActivity> =
            const { Cell::new(JsonlFamilyScannerActivity {
                worker_count: 0,
                sources_started: 0,
                sources_completed: 0,
                peak_active_scanners: 0,
            }) };
    }

    pub(crate) fn jsonl_family_scanner_activity() -> JsonlFamilyScannerActivity {
        FAMILY_SCANNER_ACTIVITY.get()
    }

    pub(in super::super) struct JsonlFamilyScannerProbe {
        sources_started: AtomicUsize,
        sources_completed: AtomicUsize,
        active_scanners: AtomicUsize,
        peak_active_scanners: AtomicUsize,
        rendezvous_arrivals: AtomicUsize,
        rendezvous_target: usize,
        rendezvous: Barrier,
    }

    impl JsonlFamilyScannerProbe {
        pub(in super::super) fn enter(&self) -> JsonlFamilyActiveScanner<'_> {
            self.sources_started.fetch_add(1, Ordering::SeqCst);
            let active = self
                .active_scanners
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            self.peak_active_scanners
                .fetch_max(active, Ordering::SeqCst);
            if self.rendezvous_arrivals.fetch_add(1, Ordering::SeqCst) < self.rendezvous_target {
                self.rendezvous.wait();
            }
            JsonlFamilyActiveScanner { probe: self }
        }

        fn snapshot(&self, worker_count: usize) -> JsonlFamilyScannerActivity {
            debug_assert_eq!(self.active_scanners.load(Ordering::SeqCst), 0);
            JsonlFamilyScannerActivity {
                worker_count,
                sources_started: self.sources_started.load(Ordering::SeqCst),
                sources_completed: self.sources_completed.load(Ordering::SeqCst),
                peak_active_scanners: self.peak_active_scanners.load(Ordering::SeqCst),
            }
        }
    }

    pub(in super::super) struct JsonlFamilyActiveScanner<'probe> {
        probe: &'probe JsonlFamilyScannerProbe,
    }

    impl Drop for JsonlFamilyActiveScanner<'_> {
        fn drop(&mut self) {
            self.probe.sources_completed.fetch_add(1, Ordering::SeqCst);
            self.probe.active_scanners.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub(in super::super) fn jsonl_family_scanner_probe(
        worker_count: usize,
    ) -> Option<Arc<JsonlFamilyScannerProbe>> {
        FAMILY_SCANNER_WORKERS_OVERRIDE.with(|workers| {
            workers.get().map(|_| {
                let rendezvous_target = worker_count.clamp(1, 4);
                Arc::new(JsonlFamilyScannerProbe {
                    sources_started: AtomicUsize::new(0),
                    sources_completed: AtomicUsize::new(0),
                    active_scanners: AtomicUsize::new(0),
                    peak_active_scanners: AtomicUsize::new(0),
                    rendezvous_arrivals: AtomicUsize::new(0),
                    rendezvous_target,
                    rendezvous: Barrier::new(rendezvous_target),
                })
            })
        })
    }

    pub(in super::super) fn record_jsonl_family_scanner_activity(
        worker_count: usize,
        probe: Option<&JsonlFamilyScannerProbe>,
    ) {
        FAMILY_SCANNER_ACTIVITY.set(
            probe.map_or_else(JsonlFamilyScannerActivity::default, |probe| {
                probe.snapshot(worker_count)
            }),
        );
    }

    pub(crate) fn with_family_scanner_workers<T>(workers: usize, run: impl FnOnce() -> T) -> T {
        struct Restore(Option<usize>);

        impl Drop for Restore {
            fn drop(&mut self) {
                FAMILY_SCANNER_WORKERS_OVERRIDE.set(self.0);
            }
        }

        let previous = FAMILY_SCANNER_WORKERS_OVERRIDE.replace(Some(workers));
        let _restore = Restore(previous);
        FAMILY_SCANNER_ACTIVITY.set(JsonlFamilyScannerActivity::default());
        run()
    }
}

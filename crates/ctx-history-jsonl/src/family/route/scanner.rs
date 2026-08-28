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
}

impl JsonlFamilySemanticPage {
    pub fn new(records: Vec<CoreRecord>) -> Self {
        Self { records }
    }

    /// Splits projected records into publication pages without changing their
    /// order, fitting an oversized individual record to the shared
    /// identity-preserving omission policy before calculating page bounds.
    pub fn split_bounded<E: JsonlFamilyError>(
        mut records: Vec<CoreRecord>,
    ) -> JsonlResult<Vec<Self>, E> {
        fit_semantic_page_records::<E>(&mut records)?;
        let encoded_lengths = semantic_record_encoded_lengths::<E>(&records)?;
        let ranges = bounded_semantic_page_ranges::<E>(&encoded_lengths)?;
        let mut records = records.into_iter();
        Ok(ranges
            .into_iter()
            .map(|range| Self::new(records.by_ref().take(range.len()).collect()))
            .collect())
    }

    pub fn records(&self) -> &[CoreRecord] {
        &self.records
    }

    pub(super) fn into_records(self) -> Vec<CoreRecord> {
        self.records
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
        if encoded_bytes > PAGE_MAX_BYTES {
            return Err(E::invalid_payload(format!(
                "JSONL semantic page exceeds the {PAGE_MAX_BYTES} byte limit"
            )));
        }
        Ok(self.records)
    }
}

fn fit_semantic_page_records<E: JsonlFamilyError>(
    records: &mut [CoreRecord],
) -> JsonlResult<(), E> {
    for record in records {
        crate::fit_jsonl_semantic_page_record(record)
            .map_err(|error| E::invalid_payload(error.to_string()))?;
    }
    Ok(())
}

fn semantic_record_encoded_lengths<E: JsonlFamilyError>(
    records: &[CoreRecord],
) -> JsonlResult<Vec<usize>, E> {
    records
        .iter()
        .map(|record| {
            record
                .encoded_json_len()
                .map_err(|error| E::invalid_payload(error.to_string()))
        })
        .collect()
}

fn bounded_semantic_page_ranges<E: JsonlFamilyError>(
    encoded_lengths: &[usize],
) -> JsonlResult<Vec<std::ops::Range<usize>>, E> {
    let mut ranges = Vec::new();
    let mut page_start = 0_usize;
    let mut page_bytes = 0_usize;
    for (index, &encoded_length) in encoded_lengths.iter().enumerate() {
        if encoded_length > PAGE_MAX_BYTES {
            return Err(E::invalid_payload(format!(
                "JSONL semantic record exceeds the {PAGE_MAX_BYTES} byte limit after fitting"
            )));
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
    ranges.push(page_start..encoded_lengths.len());
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
        derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput, NativeItemKey,
        NativeSessionKey, SessionIdentityInput, SourceAnchor,
    };
    use ctx_history_source_io::SourceIoError;

    fn source() -> SourceKey {
        SourceKey::derive(
            CaptureProvider::Pi.as_str(),
            "jsonl-semantic-page-test",
            "v1",
            1,
            SourceAnchor::provider_native("session", TypedKey::utf8("page.jsonl").unwrap())
                .unwrap(),
        )
        .unwrap()
    }

    fn record(source: &SourceKey, ordinal: u64, body: String) -> CoreRecord {
        let session_key = NativeSessionKey::native_id(
            "session",
            TypedKey::utf8("semantic-page-session").unwrap(),
        )
        .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "session",
            native_session_key: &session_key,
        })
        .unwrap();
        let item_key = NativeItemKey::native_id("event", TypedKey::U64(ordinal)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: "event",
            native_item_key: &item_key,
            subrecord_selector: None,
        })
        .unwrap();
        CoreRecord::new_selected(
            event_id,
            session_id,
            source.clone(),
            ordinal,
            "event",
            "jsonl-semantic-page-test-v1",
            body,
        )
        .unwrap()
    }

    fn record_at_exact_encoded_size(
        source: &SourceKey,
        ordinal: u64,
        encoded_size: usize,
    ) -> CoreRecord {
        let overhead = record(source, ordinal, "x".to_owned())
            .encoded_json_len()
            .unwrap()
            .saturating_sub(1);
        record(source, ordinal, "x".repeat(encoded_size - overhead))
    }

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
    fn exact_cap_record_is_selected_but_cap_plus_one_is_identity_preserving_omitted() {
        let source = source();
        let mut exact = record_at_exact_encoded_size(&source, 1, PAGE_MAX_BYTES);
        let mut oversized = record_at_exact_encoded_size(&source, 2, PAGE_MAX_BYTES + 1);
        let expected_identity = (
            oversized.event_id,
            oversized.session_id,
            oversized.source.clone(),
            oversized.event_sequence,
            oversized.event_type.clone(),
            oversized.parser_revision.clone(),
        );

        crate::fit_jsonl_semantic_page_record(&mut exact).unwrap();
        crate::fit_jsonl_semantic_page_record(&mut oversized).unwrap();

        assert_eq!(exact.encoded_json_len().unwrap(), PAGE_MAX_BYTES);
        assert!(matches!(
            exact.content.policy_status,
            ctx_history_core::CoreContentPolicyStatus::Selected
        ));
        assert_eq!(
            (
                oversized.event_id,
                oversized.session_id,
                oversized.source.clone(),
                oversized.event_sequence,
                oversized.event_type.clone(),
                oversized.parser_revision.clone(),
            ),
            expected_identity
        );
        assert!(matches!(
            &oversized.content.policy_status,
            ctx_history_core::CoreContentPolicyStatus::Omitted { reason }
                if reason == crate::JSONL_SEMANTIC_PAGE_CONTENT_OMISSION_REASON
        ));
        assert!(oversized.content.normalized_body.is_none());
        assert!(oversized.content.structured_content.is_none());
        assert!(oversized.content.discovery_exclusion.is_none());
        assert!(oversized.content.activity.is_none());
        assert!(oversized.encoded_json_len().unwrap() <= PAGE_MAX_BYTES);
    }

    #[test]
    fn fitting_preserves_valid_siblings_and_is_deterministic() {
        let source = source();
        let before = record(&source, 1, "before".to_owned());
        let oversized = record_at_exact_encoded_size(&source, 2, PAGE_MAX_BYTES + 1);
        let after = record(&source, 3, "after".to_owned());
        let expected_ids = [before.event_id, oversized.event_id, after.event_id];

        let first = JsonlFamilySemanticPage::split_bounded::<SourceIoError>(vec![
            before.clone(),
            oversized.clone(),
            after.clone(),
        ])
        .unwrap();
        let replay =
            JsonlFamilySemanticPage::split_bounded::<SourceIoError>(vec![before, oversized, after])
                .unwrap();
        let first_records = first
            .iter()
            .flat_map(|page| page.records())
            .collect::<Vec<_>>();
        let replay_records = replay
            .iter()
            .flat_map(|page| page.records())
            .collect::<Vec<_>>();

        assert_eq!(
            first_records
                .iter()
                .map(|record| record.event_id)
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(first_records, replay_records);
        assert_eq!(
            first_records[0].content.normalized_body.as_deref(),
            Some("before")
        );
        assert_eq!(
            first_records[2].content.normalized_body.as_deref(),
            Some("after")
        );
        let mut fitted_twice = first_records[1].clone();
        crate::fit_jsonl_semantic_page_record(&mut fitted_twice).unwrap();
        assert_eq!(fitted_twice, *first_records[1]);
    }

    #[test]
    fn aggregate_over_byte_and_record_caps_splits_without_loss() {
        let source = source();
        let records = (0..=PAGE_MAX_RECORDS)
            .map(|ordinal| record(&source, ordinal as u64, "x".repeat(128 * 1024)))
            .collect::<Vec<_>>();
        let expected_ids = records
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>();
        assert!(
            records
                .iter()
                .map(|record| record.encoded_json_len().unwrap())
                .sum::<usize>()
                > PAGE_MAX_BYTES
        );

        let pages = JsonlFamilySemanticPage::split_bounded::<SourceIoError>(records).unwrap();
        let actual_ids = pages
            .iter()
            .flat_map(|page| page.records())
            .map(|record| record.event_id)
            .collect::<Vec<_>>();

        assert!(pages.len() > 1);
        assert_eq!(actual_ids, expected_ids);
        assert!(pages.iter().all(|page| {
            page.records().len() <= PAGE_MAX_RECORDS
                && page
                    .records()
                    .iter()
                    .map(|record| record.encoded_json_len().unwrap())
                    .sum::<usize>()
                    <= PAGE_MAX_BYTES
        }));
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

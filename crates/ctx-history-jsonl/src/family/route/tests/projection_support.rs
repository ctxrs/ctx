use super::*;

pub(super) struct EmissionTestAdapter {
    pub(super) project_fanout: usize,
    pub(super) finish_fanout: usize,
    pub(super) admitted: Option<Arc<AtomicUsize>>,
    pub(super) observed_before_65: Option<Arc<AtomicUsize>>,
}

pub(super) struct EmissionTestProjector {
    pub(super) source: SourceKey,
    pub(super) project_fanout: usize,
    pub(super) finish_fanout: usize,
    pub(super) admitted: Option<Arc<AtomicUsize>>,
    pub(super) observed_before_65: Option<Arc<AtomicUsize>>,
}

impl EmissionTestAdapter {
    pub(super) fn ordinary() -> Self {
        Self {
            project_fanout: 1,
            finish_fanout: 0,
            admitted: None,
            observed_before_65: None,
        }
    }
}

pub(super) fn emission_test_record(source: &SourceKey, ordinal: u64) -> Result<CoreRecord> {
    emission_test_typed_record(source, ordinal, "message")
}

pub(super) fn emission_test_typed_record(
    source: &SourceKey,
    ordinal: u64,
    event_type: &'static str,
) -> Result<CoreRecord> {
    let session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8("session").map_err(test_contract_error)?,
    )
    .map_err(test_contract_error)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .map_err(test_contract_error)?;
    let native_item_key = NativeItemKey::native_id(event_type, TypedKey::U64(ordinal))
        .map_err(test_contract_error)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: event_type,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(test_contract_error)?;
    let mut projected = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        ordinal,
        event_type,
        "jsonl-emission-test-v1",
        "bounded",
    )
    .map_err(test_contract_error)?;
    projected.provider_session_id = Some("session".to_owned());
    projected.native_event_id = Some(TypedKey::U64(ordinal));
    projected.occurred_at_unix_ms = Some(ordinal as i64);
    projected.role = Some("user".to_owned());
    Ok(projected)
}

impl JsonlFamilyProjector for EmissionTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let base = record
            .evidence()
            .physical_ordinal()
            .checked_mul(1_000)
            .ok_or(CaptureError::SystemInvariant(
                "emission-test ordinal overflowed",
            ))?;
        self.emit_fanout(base, self.project_fanout, emit)
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        self.emit_fanout(1_000_000, self.finish_fanout, emit)
    }
}

impl EmissionTestProjector {
    pub(super) fn emit_fanout(
        &self,
        base: u64,
        count: usize,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        for index in 0..count {
            if index == 64 {
                if let (Some(admitted), Some(observed)) =
                    (self.admitted.as_ref(), self.observed_before_65.as_ref())
                {
                    observed.store(admitted.load(Ordering::SeqCst), Ordering::SeqCst);
                }
            }
            let ordinal = base
                .checked_add(index as u64)
                .ok_or(CaptureError::SystemInvariant(
                    "emission-test fanout overflowed",
                ))?;
            emit(emission_test_record(&self.source, ordinal)?)?;
        }
        Ok(())
    }
}

impl_standard_jsonl_test_adapter!(
    EmissionTestAdapter,
    "emission-test-parser-v1",
    JsonlFamilyAppendMode::CertifiedSuffix,
    |adapter, leaf, _source_file, _imported_at| {
        Ok(Box::new(EmissionTestProjector {
            source: leaf.source().clone(),
            project_fanout: adapter.project_fanout,
            finish_fanout: adapter.finish_fanout,
            admitted: adapter.admitted.clone(),
            observed_before_65: adapter.observed_before_65.clone(),
        }))
    }
);

pub(super) struct RecordRejectionTestAdapter;

pub(super) struct RecordRejectionTestProjector {
    source: SourceKey,
    rejections: JsonlRecordRejections,
}

impl JsonlFamilyAdapter for RecordRejectionTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "record-rejection-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Ok(Box::new(RecordRejectionTestProjector {
            source: leaf.source().clone(),
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::Pi,
                leaf.source_path().display().to_string(),
            ),
        }))
    }
}

impl JsonlFamilyProjector for RecordRejectionTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if record.bytes().iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let detail = if record.oversized() {
            Some("test record exceeds the JSONL record bound".to_owned())
        } else {
            serde_json::from_slice::<serde_json::Value>(record.bytes())
                .err()
                .map(|error| format!("malformed test JSONL: {error}"))
        };
        if let Some(detail) = detail {
            self.rejections.malformed(record, detail);
            return Ok(());
        }
        emit(emission_test_record(
            &self.source,
            record.evidence().physical_ordinal(),
        )?)
    }

    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopedPreflightTestBehavior {
    WrongSource,
    GenericInternal,
    PostStagingFailure,
}

pub(super) struct ScopedPreflightTestAdapter {
    pub(super) behavior: ScopedPreflightTestBehavior,
}

struct ScopedPreflightTestProjector {
    behavior: ScopedPreflightTestBehavior,
    source: SourceKey,
    wrong_source: SourceKey,
}

impl JsonlFamilyProjector for ScopedPreflightTestProjector {
    type Runtime = TestJsonlRuntime;

    fn preflight_with_failure_scope(
        &mut self,
        reader: &mut JsonlReader,
        _certified_prefix_end: Option<u64>,
    ) -> std::result::Result<bool, JsonlFamilyProjectorPreflightError<CaptureError>> {
        match self.behavior {
            ScopedPreflightTestBehavior::WrongSource => {
                return Err(JsonlFamilyProjectorPreflightError::logical_source_failure(
                    self.wrong_source.clone(),
                    "wrong-source preflight claim",
                ));
            }
            ScopedPreflightTestBehavior::GenericInternal => {
                return Err(JsonlFamilyProjectorPreflightError::internal(
                    CaptureError::InvalidPayload("generic preflight failure".to_owned()),
                ));
            }
            ScopedPreflightTestBehavior::PostStagingFailure => {}
        }
        while reader
            .visit_page(&mut |_record| -> Result<()> { Ok(()) })?
            .is_some()
        {}
        Ok(false)
    }

    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        for ordinal in 0..65 {
            emit(emission_test_record(&self.source, ordinal)?)?;
        }
        Err(CaptureError::InvalidPayload(
            "post-staging generic failure".to_owned(),
        ))
    }
}

impl_standard_jsonl_test_adapter!(
    ScopedPreflightTestAdapter,
    "scoped-preflight-test-parser-v1",
    JsonlFamilyAppendMode::ProjectorPreflight(true),
    |adapter, leaf, _source_file, _imported_at| {
        let wrong_source = SourceKey::derive(
            CaptureProvider::Pi.as_str(),
            TEST_SOURCE_FORMAT,
            TEST_SCHEMA,
            1,
            SourceAnchor::CatalogLineage([0xfe; 32]),
        )
        .map_err(test_contract_error)?;
        Ok(Box::new(ScopedPreflightTestProjector {
            behavior: adapter.behavior,
            source: leaf.source().clone(),
            wrong_source,
        }))
    }
);
pub(super) struct FramingPolicyTestAdapter {
    pub(super) projected: Arc<Mutex<Vec<Vec<u8>>>>,
    pub(super) record_framing: JsonlRecordFraming,
}

pub(super) struct FramingPolicyTestProjector {
    pub(super) projected: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl JsonlFamilyProjector for FramingPolicyTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        self.projected.lock().unwrap().push(record.bytes().to_vec());
        Ok(())
    }
}

impl_standard_jsonl_test_adapter!(
    FramingPolicyTestAdapter,
    "framing-policy-test-parser-v1",
    JsonlFamilyAppendMode::CertifiedSuffix,
    |adapter, _leaf, _source_file, _imported_at| {
        Ok(Box::new(FramingPolicyTestProjector {
            projected: Arc::clone(&adapter.projected),
        }))
    },
    |adapter| adapter.record_framing
);

#[derive(Default)]
pub(super) struct CheckpointTestAdapter {
    pub(super) projection_modes: Mutex<Vec<JsonlFamilyProjectionMode>>,
    pub(super) fixed_checkpoint_bytes: Option<usize>,
}

pub(super) struct OptimizedLeafTestAdapter {
    pub(super) scans: AtomicUsize,
    pub(super) emit_wrong_source: bool,
    pub(super) emit_progress_records: bool,
}

impl JsonlFamilyAdapter for OptimizedLeafTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "optimized-leaf-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "optimized leaf test must not construct the generic projector",
        ))
    }

    fn scan_optimized_leaf(
        &self,
        leaf: &JsonlFamilyLeaf,
        _base: Option<&CertifiedSource>,
        _base_event_lookup: &IndexBaseEventLookup,
        _worker: &mut JsonlFamilyWorkerContext,
        emit_page: &mut dyn FnMut(JsonlFamilyPublication, u64, Vec<CoreRecord>) -> Result<()>,
    ) -> Result<Option<JsonlFamilyOptimizedLeafOutcome>> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        drop(leaf.open_verified()?);
        let records = if self.emit_wrong_source {
            let wrong_source = SourceKey::derive(
                self.provider().as_str(),
                TEST_SOURCE_FORMAT,
                TEST_SCHEMA,
                1,
                SourceAnchor::provider_native(
                    "wrong-optimized-source",
                    TypedKey::utf8("wrong").map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?,
            )
            .map_err(test_contract_error)?;
            vec![emission_test_record(&wrong_source, 0)?]
        } else if self.emit_progress_records {
            vec![
                emission_test_record(leaf.source(), 1)?,
                emission_test_record(leaf.source(), 2)?,
                emission_test_typed_record(leaf.source(), 3, "tool_call")?,
            ]
        } else {
            Vec::new()
        };
        let source_bytes = if self.emit_progress_records {
            PROGRESS_TEST_RECORDS
        } else {
            TEST_RECORD
        };
        let retained_records = u64::try_from(records.len()).unwrap_or(u64::MAX);
        let complete_records = if self.emit_progress_records { 3 } else { 1 };
        let completed_bytes = if self.emit_progress_records {
            source_bytes.len() as u64
        } else {
            0
        };
        emit_page(JsonlFamilyPublication::Replace, completed_bytes, records)?;
        let observation =
            scanner::source_observation::<CaptureError>(leaf.source(), leaf.observation())?;
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            self.parser_revision(),
            Sha256::digest(source_bytes).into(),
            ScannedSourceCounts {
                complete_records,
                retained_records,
                rejected_records: 0,
                ignored_records: complete_records.saturating_sub(retained_records),
                indexed_documents: retained_records,
                certified_bytes: source_bytes.len() as u64,
            },
        )
        .map_err(test_contract_error)?;
        let terminal_proof = JsonlFamilyTerminalProof::exact_file(self, leaf, &certificate)?;
        Ok(Some(JsonlFamilyOptimizedLeafOutcome::replacement(
            certificate,
            terminal_proof,
        )))
    }
}

pub(super) struct CheckpointTestProjector {
    pub(super) projected_records: u64,
    pub(super) resumed: bool,
    pub(super) fixed_checkpoint_bytes: Option<usize>,
}

impl JsonlFamilyProjector for CheckpointTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.resumed && self.projected_records != record.evidence().physical_ordinal() {
            return Err(CaptureError::InvalidPayload(
                "opaque checkpoint resumed from the wrong JSONL ordinal".to_owned(),
            ));
        }
        self.projected_records =
            self.projected_records
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "checkpoint test record count overflowed",
                ))?;
        Ok(())
    }

    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        self.fixed_checkpoint_bytes.map_or_else(
            || Ok(Some(TypedKey::U64(self.projected_records))),
            |bytes| {
                TypedKey::utf8("\"".repeat(bytes))
                    .map(Some)
                    .map_err(test_contract_error)
            },
        )
    }
}

impl JsonlFamilyAdapter for CheckpointTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "checkpoint-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Ok(Box::new(CheckpointTestProjector {
            projected_records: 0,
            resumed: false,
            fixed_checkpoint_bytes: self.fixed_checkpoint_bytes,
        }))
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        self.projection_modes.lock().unwrap().push(mode);
        let Some(checkpoint) = checkpoint else {
            if mode == JsonlFamilyProjectionMode::Cold && base_event_lookup.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "cold checkpoint test unexpectedly received a base lookup".to_owned(),
                ));
            }
            if mode == JsonlFamilyProjectionMode::Replacement && base_event_lookup.is_none() {
                return Err(CaptureError::InvalidPayload(
                    "replacement checkpoint test did not receive a base lookup".to_owned(),
                ));
            }
            return self.projector(leaf, source_file, imported_at);
        };
        if mode != JsonlFamilyProjectionMode::CertifiedAppend || base_event_lookup.is_none() {
            return Err(CaptureError::InvalidPayload(
                "resumed checkpoint test did not receive a base lookup".to_owned(),
            ));
        }
        let TypedKey::U64(projected_records) = checkpoint else {
            return Err(CaptureError::InvalidPayload(
                "checkpoint test state is malformed".to_owned(),
            ));
        };
        Ok(Box::new(CheckpointTestProjector {
            projected_records: *projected_records,
            resumed: true,
            fixed_checkpoint_bytes: self.fixed_checkpoint_bytes,
        }))
    }
}

use super::*;

pub(super) struct ParallelTestAdapter;

pub(super) struct ParallelTestProjector;

impl JsonlFamilyProjector for ParallelTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        _record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        _emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        Ok(())
    }
}

impl_standard_jsonl_test_adapter!(
    ParallelTestAdapter,
    "parallel-test-parser-v1",
    JsonlFamilyAppendMode::CertifiedSuffix,
    |_adapter, _leaf, _source_file, _imported_at| { Ok(Box::new(ParallelTestProjector)) }
);

pub(super) struct ReplacementParallelTestAdapter;

impl_standard_jsonl_test_adapter!(
    ReplacementParallelTestAdapter,
    "replacement-parallel-test-parser-v1",
    JsonlFamilyAppendMode::Replacement,
    |_adapter, _leaf, _source_file, _imported_at| { Ok(Box::new(ParallelTestProjector)) }
);

pub(super) struct AllRejectedParallelTestAdapter {
    pub(super) reject: Arc<AtomicBool>,
}

pub(super) struct AllRejectedParallelTestProjector {
    pub(super) source: SourceKey,
    pub(super) reject: Arc<AtomicBool>,
    pub(super) rejected_records: u64,
}

impl JsonlFamilyProjector for AllRejectedParallelTestProjector {
    type Runtime = TestJsonlRuntime;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if self.reject.load(Ordering::SeqCst) {
            self.rejected_records =
                self.rejected_records
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "all-rejected test count overflowed",
                    ))?;
            return Ok(());
        }
        emit(emission_test_record(
            &self.source,
            record.evidence().physical_ordinal(),
        )?)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }
}

impl_standard_jsonl_test_adapter!(
    AllRejectedParallelTestAdapter,
    "all-rejected-parallel-test-parser-v1",
    JsonlFamilyAppendMode::Replacement,
    |adapter, leaf, _source_file, _imported_at| {
        Ok(Box::new(AllRejectedParallelTestProjector {
            source: leaf.source().clone(),
            reject: Arc::clone(&adapter.reject),
            rejected_records: 0,
        }))
    }
);

pub(super) struct IdentityRevisionTestAdapter {
    pub(super) parser_revision: &'static str,
    pub(super) revision: &'static str,
    pub(super) expected_mode: JsonlFamilyProjectionMode,
}

impl JsonlFamilyAdapter for IdentityRevisionTestAdapter {
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
        self.parser_revision
    }

    fn event_identity_revision(&self) -> &'static str {
        self.revision
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
        Ok(Box::new(ParallelTestProjector))
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
        if checkpoint.is_some()
            || mode != self.expected_mode
            || base_event_lookup.is_some() != (mode != JsonlFamilyProjectionMode::Cold)
        {
            return Err(CaptureError::InvalidPayload(
                "identity revision test received inconsistent projection context".to_owned(),
            ));
        }
        self.projector(leaf, source_file, imported_at)
    }
}

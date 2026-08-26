use super::*;

#[derive(Default)]
pub(super) struct TestWorkerServices {
    pub(super) certified_repositories: HashSet<PathBuf>,
    pub(super) full_certification_probes: usize,
    pub(super) event_time_entries: usize,
}

impl TestWorkerServices {
    pub(super) fn begin_source(&mut self) {
        self.event_time_entries = 0;
    }

    pub(super) fn attribute(&mut self, repository: &Path) -> bool {
        if self.certified_repositories.insert(repository.to_path_buf()) {
            self.full_certification_probes = self.full_certification_probes.saturating_add(1);
        }
        self.event_time_entries = self.event_time_entries.saturating_add(1);
        true
    }

    pub(super) fn full_certification_probe_count(&self) -> usize {
        self.full_certification_probes
    }

    pub(super) fn event_time_cache_len(&self) -> usize {
        self.event_time_entries
    }
}

pub(super) struct TestJsonlRuntime;

impl JsonlFamilyRuntime for TestJsonlRuntime {
    type Error = CaptureError;
    type Lifecycle = TestLifecycle;
    type WorkerServices = TestWorkerServices;
    type RouteControl = ();

    fn begin_worker_leaf(services: &mut Self::WorkerServices) {
        services.begin_source();
    }
}

#[derive(Clone, Default)]
pub(super) struct TestBaseEventLookup {
    pub(super) events: HashSet<uuid::Uuid>,
}

impl BaseEventLookup for TestBaseEventLookup {
    type Error = CaptureError;

    fn contains(&self, event_id: uuid::Uuid) -> Result<bool> {
        Ok(self.events.contains(&event_id))
    }
}

#[derive(Clone, Default)]
pub(super) struct TestPreparation;

pub(super) struct TestPreparedRecord {
    pub(super) record: CoreRecord,
    pub(super) encoded_bytes: usize,
}

impl CorePreparationPort for TestPreparation {
    type Prepared = TestPreparedRecord;
    type Draft = CoreRecord;
    type Failure = CaptureError;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared> {
        let encoded_bytes = record
            .encode_stored()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
            .len();
        Ok(TestPreparedRecord {
            record,
            encoded_bytes,
        })
    }

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft> {
        record
            .validate_contract()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>> {
        let encoded_bytes = draft
            .encode_stored()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
            .len();
        if encoded_bytes > maximum_encoded_bytes {
            return Ok(CoreMaterialization::CapacityExceeded(Box::new(draft)));
        }
        Ok(CoreMaterialization::Prepared(TestPreparedRecord {
            record: draft,
            encoded_bytes,
        }))
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        &prepared.record.source
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared.encoded_bytes
    }

    fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
        CorePreparationFailureKind::InvalidSource
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TestSnapshot {
    pub(super) sources: Vec<CertifiedSource>,
    pub(super) route_identity: Option<SourceRouteIdentity>,
    pub(super) route_sources: Vec<SourceKey>,
    pub(super) records: Vec<CoreRecord>,
}

impl ImmutableCaptureSnapshot for TestSnapshot {
    fn sources(&self) -> &[CertifiedSource] {
        &self.sources
    }

    fn source_aggregates(&self) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>> {
        std::iter::empty()
    }

    fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
            .into_iter()
    }

    fn source_route(&self, route_identity: &SourceRouteIdentity) -> Option<CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .filter(|identity| *identity == route_identity)
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
    }
}

#[derive(Debug)]
pub(super) struct IndexCaptureCommitReceipt {
    pub(super) generation_id: String,
    pub(super) manifest: TestSnapshot,
}

impl IndexCaptureCommitReceipt {
    pub(super) fn new(receipt: CaptureCommitReceipt<TestSnapshot>) -> Self {
        let (generation_id, _, _, _, _, manifest) = receipt.into_parts();
        Self {
            generation_id,
            manifest,
        }
    }

    pub(super) fn manifest(&self) -> &TestSnapshot {
        &self.manifest
    }
}

pub(super) fn test_generations() -> &'static Mutex<HashMap<PathBuf, TestSnapshot>> {
    static GENERATIONS: OnceLock<Mutex<HashMap<PathBuf, TestSnapshot>>> = OnceLock::new();
    GENERATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) struct TestLifecycle {
    pub(super) root: PathBuf,
    pub(super) base: Option<TestSnapshot>,
    pub(super) current_source: Option<SourceKey>,
    pub(super) records: Vec<CoreRecord>,
    pub(super) certified_sources: Vec<CertifiedSource>,
    pub(super) activity: TestLifecycleActivity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TestLifecycleActivity {
    pub(super) begin_source_replacements: usize,
    pub(super) begin_source_appends: usize,
    pub(super) retained_sources: usize,
    pub(super) deleted_sources: usize,
}

impl TestLifecycle {
    pub(super) fn snapshot(&self) -> TestSnapshot {
        let mut sources = self.certified_sources.clone();
        sources.sort_by(|left, right| {
            left.observation()
                .source()
                .cmp(right.observation().source())
        });
        TestSnapshot {
            route_identity: Some(test_route_identity()),
            route_sources: sources
                .iter()
                .map(|source| source.observation().source().clone())
                .collect(),
            sources,
            records: self.records.clone(),
        }
    }

    pub(super) fn base_event_identity_lookup(&self) -> TestBaseEventLookup {
        self.base_event_lookup()
    }

    pub(super) fn activity(&self) -> TestLifecycleActivity {
        self.activity
    }

    pub(super) fn commit_receipt(self) -> CaptureCommitReceipt<TestSnapshot> {
        let root = self.root.clone();
        let snapshot = self.snapshot();
        let indexed_documents = snapshot
            .sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum();
        let certified_source_bytes = snapshot
            .sources
            .iter()
            .map(|source| source.counts().certified_bytes)
            .sum();
        let mut generations = test_generations().lock().unwrap();
        let next_opstamp = if self.base.as_ref() == Some(&snapshot) {
            1
        } else {
            generations.get(&root).map_or(1, |_| 2)
        };
        let generation_id = format!("test-generation-{next_opstamp}");
        generations.insert(root, snapshot.clone());
        CaptureCommitReceipt::new(
            generation_id,
            next_opstamp,
            indexed_documents,
            snapshot.sources.len(),
            certified_source_bytes,
            snapshot,
        )
    }
}

impl CaptureLifecycleSink for TestLifecycle {
    type Error = CaptureError;
    type OpenOptions = ();
    type BaseLookup = TestBaseEventLookup;
    type Preparation = TestPreparation;
    type PinnedAppendBase = CertifiedSource;
    type CommittedSnapshot = TestSnapshot;
    type VerifiedPublication = ();
    type Snapshot<'a> = TestSnapshot;

    fn invariant_error(detail: &'static str) -> Self::Error {
        CaptureError::SystemInvariant(detail)
    }

    fn open(root: &Path, _options: Self::OpenOptions) -> Result<CaptureLifecycleOpenOutcome<Self>> {
        let base = test_generations().lock().unwrap().get(root).cloned();
        let records = base
            .as_ref()
            .map(|snapshot| snapshot.records.clone())
            .unwrap_or_default();
        Ok(CaptureLifecycleOpenOutcome::Ready(Self {
            root: root.to_path_buf(),
            base,
            current_source: None,
            records,
            certified_sources: Vec::new(),
            activity: TestLifecycleActivity::default(),
        }))
    }

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
        self.base.clone()
    }

    fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.base
            .as_ref()?
            .sources
            .iter()
            .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
    }

    fn pinned_append_base(
        &self,
        _route_identity: &SourceRouteIdentity,
        source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase> {
        self.base_source(source).cloned()
    }

    fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource {
        base
    }

    fn base_event_lookup(&self) -> Self::BaseLookup {
        TestBaseEventLookup {
            events: self
                .base
                .iter()
                .flat_map(|snapshot| {
                    snapshot
                        .records
                        .iter()
                        .map(|record| record.event_id.as_uuid())
                })
                .collect(),
        }
    }

    fn core_preparation(&self) -> Self::Preparation {
        TestPreparation
    }

    fn set_route_plan(
        &mut self,
        _selected: BTreeSet<SourceRouteIdentity>,
        _carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> Result<()> {
        Ok(())
    }

    fn begin_route_stage(&mut self, _route_identity: SourceRouteIdentity) -> Result<()> {
        Ok(())
    }

    fn retain_unstaged_route_members(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<()> {
        Ok(())
    }

    fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
        false
    }

    fn register_route_revalidation(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _revalidate: impl Fn() -> bool + Send + 'static,
    ) -> Result<()> {
        Ok(())
    }

    fn visit_revalidation_targets<E>(
        &self,
        mut visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> std::result::Result<(), E>,
    ) -> Result<std::result::Result<(), E>> {
        for source in &self.certified_sources {
            if let Err(error) = visit(CaptureRevalidationTarget::Source(source)) {
                return Ok(Err(error));
            }
        }
        Ok(Ok(()))
    }

    fn finish_route_stage(&mut self, _route_identity: &SourceRouteIdentity) -> Result<()> {
        Ok(())
    }

    fn rollback_route_stage(&mut self, _route_identity: &SourceRouteIdentity) -> Result<()> {
        self.current_source = None;
        Ok(())
    }

    fn authorize_carried_route_retirement(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<()> {
        Ok(())
    }

    fn retire_carried_route(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>> {
        Ok(Vec::new())
    }

    fn begin_source_replace(&mut self, source: SourceKey) -> Result<()> {
        self.activity.begin_source_replacements += 1;
        self.records
            .retain(|record| !record.source.exact_descriptor_eq(&source));
        self.current_source = Some(source);
        Ok(())
    }

    fn begin_source_append(&mut self, source: SourceKey) -> Result<&CertifiedSource> {
        self.activity.begin_source_appends += 1;
        self.current_source = Some(source.clone());
        self.base_source(&source)
            .ok_or(CaptureError::SystemInvariant("append source has no base"))
    }

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> Result<&CertifiedSource> {
        self.begin_source_append(base.observation().source().clone())
    }

    fn add_prepared(&mut self, prepared: TestPreparedRecord) -> Result<()> {
        self.records.push(prepared.record);
        Ok(())
    }

    fn certify_source(&mut self, certificate: CertifiedSource) -> Result<()> {
        self.certified_sources.push(certificate);
        self.current_source = None;
        Ok(())
    }

    fn certify_source_append(&mut self, append: CertifiedSourceAppend) -> Result<()> {
        self.certified_sources.push(append.into_current());
        self.current_source = None;
        Ok(())
    }

    fn retain_source(&mut self, certificate: CertifiedSource) -> Result<()> {
        self.activity.retained_sources += 1;
        self.certified_sources.push(certificate);
        Ok(())
    }

    fn certify_complete_inventory(&mut self, _inventory: CertifiedSourceInventory) -> Result<()> {
        Ok(())
    }

    fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        _inventory: CertifiedSourceInventory,
    ) -> Result<()> {
        self.activity.deleted_sources = self.activity.deleted_sources.saturating_add(1);
        self.records
            .retain(|record| !record.source.exact_descriptor_eq(deletion.source()));
        Ok(())
    }

    fn carry_failed_route(&mut self, _route_identity: &SourceRouteIdentity) -> Result<bool> {
        Ok(false)
    }

    fn observe_missing_route(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _observed_at_unix_ms: u64,
        _revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> Result<()> {
        Ok(())
    }

    fn set_present_routes(
        &mut self,
        _routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> Result<()> {
        Ok(())
    }

    fn commit<F, I>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
    ) -> Result<CaptureCommitReceipt<TestSnapshot>>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(self.commit_receipt())
    }

    fn commit_with_metadata<F, I, M>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
        metadata_factory: M,
    ) -> Result<CaptureCommitOutcome<TestSnapshot, ()>>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: for<'a> FnOnce(CapturePublicationContext<'a, Self::Snapshot<'a>>) -> Result<Vec<u8>>,
    {
        let snapshot = self.snapshot();
        metadata_factory(CapturePublicationContext::new("test-generation", snapshot))?;
        Ok(CaptureCommitOutcome::new(
            self.commit_receipt(),
            CapturePublicationDisposition::Published,
            VerifiedCapture::new(()),
        ))
    }
}

macro_rules! capture_test_generation_with_resident {
    ($resident:expr, $adapter:expr, $root:expr, $index_root:expr, $workers:expr, $capture:expr) => {{
        capture_test_generation_with_resident!(
            $resident,
            $adapter,
            $root,
            $index_root,
            $workers,
            SourceBackedReconciliationDemand::Incremental,
            $capture
        )
    }};
    ($resident:expr, $adapter:expr, $root:expr, $index_root:expr, $workers:expr, $demand:expr, $capture:expr) => {{
        let mut writer = match IndexCaptureLifecycle::open($index_root, ()).unwrap() {
            CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
            CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
                panic!("test lifecycle unexpectedly requires recovery")
            }
        };
        let mut owners = HashMap::new();
        let mut complete_inventories = Vec::new();
        let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
        let mut record_rejections = SourceBackedRecordRejections::default();
        let result = {
            let mut applied_removals = Vec::new();
            let mut sink = SourceBackedGenerationSink::new(
                &mut writer,
                &mut owners,
                &mut complete_inventories,
                &mut applied_removals,
                0,
                test_route_identity(),
                None,
                SourceBackedRouteResources::production($workers)
                    .with_reconciliation_demand($demand),
                &mut logical_source_failures,
                &mut record_rejections,
                None,
                None,
                None,
            );
            with_family_scanner_workers($workers, || $capture($resident, &mut sink))
        };
        (writer, result)
    }};
}

pub(super) use capture_test_generation_with_resident;

macro_rules! capture_test_generation {
    ($adapter:expr, $root:expr, $index_root:expr, $workers:expr, $capture:expr) => {{
        capture_test_generation!(
            $adapter,
            $root,
            $index_root,
            $workers,
            SourceBackedReconciliationDemand::Incremental,
            $capture
        )
    }};
    ($adapter:expr, $root:expr, $index_root:expr, $workers:expr, $demand:expr, $capture:expr) => {{
        let resident = Mutex::new(FamilyResident::default());
        let (writer, result) = capture_test_generation_with_resident!(
            &resident,
            $adapter,
            $root,
            $index_root,
            $workers,
            $demand,
            $capture
        );
        (writer, resident, result)
    }};
}

pub(super) fn capture_test_generation_without_commit(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> TestLifecycle {
    let (writer, _resident, result) =
        capture_test_generation!(adapter, root, index_root, workers, |resident, sink| {
            capture(adapter, root, resident, sink)
        });
    result.unwrap();
    writer
}

pub(super) use capture_test_generation;

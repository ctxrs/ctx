use std::{
    cell::{Cell, RefCell},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture_model::{
    DiscoveryReport, ProviderCatalogSupport, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;
use ctx_history_refresh::{
    explicit_source_catalog_authority_for_test, explicit_source_path_symlink_metadata,
    ExplicitSourceCatalogRouteBinding, ExplicitSourceCatalogUpsert, ExplicitSourcePathMissing,
    RefreshSelection, SourceBackedRefreshCurrent, SourceBackedRefreshReceipt,
    SourceBackedRefreshRecordRejection, SourceBackedRefreshRouteResult,
    SourceBackedRefreshSourceFailure,
};

use crate::{
    run_ingest, CaptureAdmissionPort, HistorySourcePluginSource, ImportPathMissingDuringRefresh,
    ImportPathNotFound, IngestProgressPort, IngestPublication, IngestRefreshPort, IngestRequest,
    IngestSourceOutcome, ProviderSelectionGuidance, SourceDiscoveryPort, SourceStats,
};

const ROUTE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SOURCE_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct FakeHost {
    events: RefCell<Vec<String>>,
    all_discovery: DiscoveryReport,
    provider_discovery: DiscoveryReport,
    exact_source: ProviderSource,
    publication: Option<IngestPublication>,
    refresh_error: Option<&'static str>,
    refresh_path_missing: bool,
    progress_error: Option<&'static str>,
    admission_error: Option<&'static str>,
    remove_source_during_admission: bool,
    lineage: [u8; 32],
    refresh_calls: Cell<usize>,
    admission_calls: Cell<usize>,
    all_discovery_calls: Cell<usize>,
    provider_discovery_calls: Cell<usize>,
    source_identity_calls: Cell<usize>,
    relocate_from: RefCell<Option<PathBuf>>,
    refresh_selections: RefCell<Vec<RefreshSelection>>,
}

impl FakeHost {
    fn new(source_path: PathBuf, publication: IngestPublication) -> Self {
        Self {
            events: RefCell::new(Vec::new()),
            all_discovery: DiscoveryReport::default(),
            provider_discovery: DiscoveryReport::default(),
            exact_source: provider_source(source_path, ProviderSourceStatus::Available),
            publication: Some(publication),
            refresh_error: None,
            refresh_path_missing: false,
            progress_error: None,
            admission_error: None,
            remove_source_during_admission: false,
            lineage: [7; 32],
            refresh_calls: Cell::new(0),
            admission_calls: Cell::new(0),
            all_discovery_calls: Cell::new(0),
            provider_discovery_calls: Cell::new(0),
            source_identity_calls: Cell::new(0),
            relocate_from: RefCell::new(None),
            refresh_selections: RefCell::new(Vec::new()),
        }
    }

    fn push(&self, event: &str) {
        self.events.borrow_mut().push(event.to_owned());
    }

    fn lineage_hex(&self) -> String {
        self.lineage
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

impl SourceDiscoveryPort for FakeHost {
    fn discover_all(&self) -> Result<DiscoveryReport> {
        self.push("discover_all");
        self.all_discovery_calls
            .set(self.all_discovery_calls.get() + 1);
        Ok(self.all_discovery.clone())
    }

    fn discover_provider(&self, _: CaptureProvider) -> Result<DiscoveryReport> {
        self.push("discover_provider");
        self.provider_discovery_calls
            .set(self.provider_discovery_calls.get() + 1);
        Ok(self.provider_discovery.clone())
    }

    fn provider_selection_guidance(&self, provider: CaptureProvider) -> ProviderSelectionGuidance {
        ProviderSelectionGuidance {
            display_name: provider.as_str().to_owned(),
            manual_path_command: format!("select {provider}"),
        }
    }
}

impl CaptureAdmissionPort for FakeHost {
    fn protect_data_root(&mut self, _: &Path) -> Result<()> {
        self.push("protect_data_root");
        Ok(())
    }

    fn explicit_source(
        &self,
        _: &Path,
        _: &Path,
        _: Option<CaptureProvider>,
        _: bool,
    ) -> Result<ProviderSource> {
        self.push("explicit_source");
        Ok(self.exact_source.clone())
    }

    fn prepare_plugin(
        &mut self,
        source: &HistorySourcePluginSource,
        _: bool,
    ) -> Result<ProviderSource> {
        self.push("prepare_plugin");
        Ok(provider_source(
            source.source_path.clone().expect("plugin path"),
            ProviderSourceStatus::Available,
        ))
    }

    fn admit_exact(
        &mut self,
        _: &Path,
        source: &ProviderSource,
        relocate_from: Option<&Path>,
    ) -> Result<ExplicitSourceCatalogUpsert> {
        self.push("admit_exact");
        self.admission_calls.set(self.admission_calls.get() + 1);
        *self.relocate_from.borrow_mut() = relocate_from.map(Path::to_path_buf);
        if let Some(error) = self.admission_error {
            return Err(anyhow!(error));
        }
        if self.remove_source_during_admission {
            fs::remove_file(&source.path).unwrap();
            let error = explicit_source_path_symlink_metadata(&source.path)
                .with_context(|| format!("check explicit source path {}", source.path.display()))
                .unwrap_err();
            return Err(error);
        }
        Ok(ExplicitSourceCatalogUpsert {
            authority: explicit_source_catalog_authority_for_test(1),
            provider: source.provider,
            source_format: source.source_format,
            path: source.path.clone(),
            catalog_lineage: self.lineage,
        })
    }

    fn source_failure_identity(&self, _: &ProviderSource) -> Result<String> {
        self.push("source_failure_identity");
        self.source_identity_calls
            .set(self.source_identity_calls.get() + 1);
        Ok(SOURCE_ID.to_owned())
    }
}

impl IngestProgressPort for FakeHost {
    fn begin(&mut self, total_bytes: u64) -> Result<()> {
        self.push(&format!("begin:{total_bytes}"));
        if let Some(error) = self.progress_error {
            return Err(anyhow!(error));
        }
        Ok(())
    }

    fn catalog_exact(&mut self, _: &ProviderSource, _: SourceStats) -> Result<()> {
        self.push("catalog_exact");
        Ok(())
    }

    fn catalog_plugin(&mut self, _: &HistorySourcePluginSource) -> Result<()> {
        self.push("catalog_plugin");
        Ok(())
    }
}

impl IngestRefreshPort for FakeHost {
    fn refresh(
        &mut self,
        _: &Path,
        selection: RefreshSelection,
        _: bool,
    ) -> Result<IngestPublication> {
        let event = match &selection {
            RefreshSelection::All => "refresh:all",
            RefreshSelection::Provider(_) => "refresh:provider",
            RefreshSelection::ExactSource(_) => "refresh:exact_source",
        };
        self.push(event);
        self.refresh_selections.borrow_mut().push(selection);
        self.refresh_calls.set(self.refresh_calls.get() + 1);
        if self.refresh_path_missing {
            return Err(anyhow!("daemon terminal detail").context(ImportPathMissingDuringRefresh));
        }
        if let Some(error) = self.refresh_error {
            return Err(anyhow!(error));
        }
        self.publication
            .clone()
            .ok_or_else(|| anyhow!("missing fake publication"))
    }
}

fn provider_source(path: PathBuf, status: ProviderSourceStatus) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Codex,
        path,
        exists: true,
        source_format: "codex_sessions_jsonl_v1",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status,
        unsupported_reason: (status == ProviderSourceStatus::Unsupported)
            .then_some("unsupported fixture"),
        route_provenance: Default::default(),
    }
}

fn current() -> SourceBackedRefreshCurrent {
    SourceBackedRefreshCurrent {
        source_count: 1,
        indexed_documents: 2,
        complete_records: 3,
        retained_records: 4,
        rejected_records: 5,
        ignored_records: 6,
        certified_source_bytes: 7,
        sources_with_rejections: 1,
        removed_source_count: 0,
    }
}

fn publication(receipt: SourceBackedRefreshReceipt) -> IngestPublication {
    IngestPublication {
        request_id: Some("request-1".to_owned()),
        request_previous_generation: receipt.previous_generation.clone(),
        request_generation_changed: receipt.generation_changed,
        scanned_routes: Some(receipt.route_results.len()),
        pinned_generation: receipt.published_generation.clone(),
        policy_schema_hash: Some("policy-v1".to_owned()),
        catalog_content: std::collections::BTreeMap::new(),
        receipt: Some(receipt),
    }
}

fn receipt(
    lineage: Option<String>,
    result: SourceBackedRefreshRouteResult,
) -> SourceBackedRefreshReceipt {
    SourceBackedRefreshReceipt {
        previous_generation: Some("generation-0".to_owned()),
        published_generation: "generation-1".to_owned(),
        generation_changed: true,
        published_explicit_source_catalog: None,
        current: current(),
        catalog_route_bindings: lineage
            .map(|catalog_lineage| ExplicitSourceCatalogRouteBinding {
                catalog_lineage,
                route_identity: result.route_identity.clone(),
            })
            .into_iter()
            .collect(),
        route_results: vec![result],
        zero_source_authority: Vec::new(),
    }
}

fn exact_request(path: PathBuf) -> IngestRequest {
    IngestRequest {
        path: Some(path),
        provider: Some(CaptureProvider::Codex),
        ..IngestRequest::default()
    }
}

fn write_source(temp: &tempfile::TempDir) -> PathBuf {
    let path = temp.path().join("history.jsonl");
    fs::write(&path, b"history\n").unwrap();
    path
}

#[test]
fn unsupported_exact_source_never_initializes_progress_or_refreshes() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_source(&temp);
    let mut host = FakeHost::new(
        path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    host.exact_source.status = ProviderSourceStatus::Unsupported;
    host.exact_source.unsupported_reason = Some("unsupported fixture");

    let report = run_ingest(&exact_request(path), temp.path(), &mut host).unwrap();

    assert_eq!(report.totals.failed_sources, 1);
    assert_eq!(host.refresh_calls.get(), 0);
    assert_eq!(host.admission_calls.get(), 0);
    assert_eq!(host.source_identity_calls.get(), 1);
    assert_eq!(
        host.events.borrow().as_slice(),
        ["explicit_source", "source_failure_identity"]
    );
}

#[test]
fn exact_source_disappearance_during_source_stats_uses_the_typed_path_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let requested_path = temp.path().join("requested-history.jsonl");
    let owned_path = temp.path().join("canonical-history.jsonl");
    let mut host = FakeHost::new(
        owned_path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );

    let error = run_ingest(
        &exact_request(requested_path.clone()),
        temp.path(),
        &mut host,
    )
    .unwrap_err();
    let diagnostic = error.downcast_ref::<ImportPathNotFound>().unwrap();
    let missing = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ExplicitSourcePathMissing>())
        .unwrap();

    assert_eq!(diagnostic.path(), requested_path);
    assert_eq!(missing.path(), owned_path);
    assert_eq!(missing.source_error().kind(), std::io::ErrorKind::NotFound);
    assert!(error.chain().any(|cause| {
        cause.to_string() == format!("stat import source {}", owned_path.display())
    }));
    assert_eq!(host.events.borrow().as_slice(), ["explicit_source"]);
    assert_eq!(host.admission_calls.get(), 0);
    assert_eq!(host.refresh_calls.get(), 0);
}

#[test]
fn exact_source_disappearance_during_catalog_admission_maps_owned_to_requested_path() {
    let temp = tempfile::tempdir().unwrap();
    let requested_path = temp.path().join("requested-history.jsonl");
    let owned_path = write_source(&temp);
    let mut host = FakeHost::new(
        owned_path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    host.remove_source_during_admission = true;

    let error = run_ingest(
        &exact_request(requested_path.clone()),
        temp.path(),
        &mut host,
    )
    .unwrap_err();
    let diagnostic = error.downcast_ref::<ImportPathNotFound>().unwrap();
    let missing = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ExplicitSourcePathMissing>())
        .unwrap();

    assert_eq!(diagnostic.path(), requested_path);
    assert_eq!(missing.path(), owned_path);
    assert_eq!(missing.source_error().kind(), std::io::ErrorKind::NotFound);
    assert!(error.chain().any(|cause| {
        cause.to_string() == format!("check explicit source path {}", owned_path.display())
    }));
    assert_eq!(host.admission_calls.get(), 1);
    assert_eq!(host.refresh_calls.get(), 0);
}

#[test]
fn manual_all_safety_snapshot_precedes_root_admission_and_forwards_all_selection() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = write_source(&temp);
    let result = SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true);
    let mut host = FakeHost::new(source_path.clone(), publication(receipt(None, result)));
    host.all_discovery.sources = vec![provider_source(
        source_path,
        ProviderSourceStatus::Available,
    )];

    let request = IngestRequest {
        all: true,
        ..IngestRequest::default()
    };

    run_ingest(&request, &temp.path().join("ctx"), &mut host).unwrap();

    assert_eq!(host.all_discovery_calls.get(), 1);
    assert_eq!(host.refresh_calls.get(), 1);
    assert_eq!(
        host.events.borrow().as_slice(),
        [
            "begin:0",
            "discover_all",
            "protect_data_root",
            "refresh:all"
        ]
    );
    assert_eq!(
        host.refresh_selections.borrow().as_slice(),
        [RefreshSelection::All]
    );
}

#[test]
fn unsafe_automatic_root_fails_before_root_admission_or_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("ctx");
    let source_path = data_root.join("provider");
    let mut host = FakeHost::new(
        source_path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    host.all_discovery.sources = vec![provider_source(
        source_path,
        ProviderSourceStatus::Available,
    )];

    let error = run_ingest(&IngestRequest::default(), &data_root, &mut host).unwrap_err();

    assert!(format!("{error:#}").contains("before initializing ctx state"));
    assert_eq!(host.refresh_calls.get(), 0);
    assert_eq!(host.admission_calls.get(), 0);
    assert_eq!(host.events.borrow().as_slice(), ["begin:0", "discover_all"]);
}

#[test]
fn exact_route_admits_and_refreshes_exactly_once() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_source(&temp);
    let mut host = FakeHost::new(
        path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    let lineage = host.lineage_hex();
    host.publication = Some(publication(receipt(
        Some(lineage),
        SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
    )));

    let report = run_ingest(&exact_request(path), temp.path(), &mut host).unwrap();

    assert_eq!(host.admission_calls.get(), 1);
    assert_eq!(host.refresh_calls.get(), 1);
    assert_eq!(report.totals.imported_sources, 1);
    assert_eq!(
        host.events.borrow().as_slice(),
        [
            "explicit_source",
            "begin:8",
            "catalog_exact",
            "admit_exact",
            "refresh:exact_source"
        ]
    );
    assert_eq!(
        host.refresh_selections.borrow().as_slice(),
        [RefreshSelection::ExactSource(
            explicit_source_catalog_authority_for_test(1)
        )]
    );
}

#[test]
fn exact_failure_detail_remains_typed_for_cli_presentation() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_source(&temp);
    let mut host = FakeHost::new(
        path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    let lineage = host.lineage_hex();
    let mut failed =
        SourceBackedRefreshRouteResult::failed(ROUTE.to_owned(), "incompatible".to_owned(), false);
    failed
        .source_failures
        .push(SourceBackedRefreshSourceFailure {
            route_identity: ROUTE.to_owned(),
            source_identity: SOURCE_ID.to_owned(),
            provider: "codex".to_owned(),
            class: "incompatible".to_owned(),
            carried_forward: false,
            source_selector: path.display().to_string(),
            detail: "unsupported source schema".to_owned(),
        });
    host.publication = Some(publication(receipt(Some(lineage), failed)));

    let report = run_ingest(&exact_request(path.clone()), temp.path(), &mut host).unwrap();

    assert_eq!(
        report.first_failure_detail(),
        Some((
            path.to_str().unwrap(),
            crate::IngestFailureType::UnsupportedSchema,
            "unsupported source schema",
        ))
    );
}

#[test]
fn pinned_generation_mismatch_is_rejected_by_application() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_source(&temp);
    let mut host = FakeHost::new(
        path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    let lineage = host.lineage_hex();
    let mut mismatched = publication(receipt(
        Some(lineage),
        SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
    ));
    mismatched.pinned_generation = "different-generation".to_owned();
    host.publication = Some(mismatched);

    let error = run_ingest(&exact_request(path), temp.path(), &mut host).unwrap_err();

    assert!(error
        .to_string()
        .contains("verified publication pin carries"));
    assert_eq!(host.refresh_calls.get(), 1);
}

#[test]
fn exact_route_requires_terminal_result_for_selected_lineage() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_source(&temp);
    let mut host = FakeHost::new(
        path.clone(),
        publication(receipt(
            Some("different-lineage".to_owned()),
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );

    let error = run_ingest(&exact_request(path), temp.path(), &mut host).unwrap_err();

    assert!(error.to_string().contains("exact catalog-lineage result"));
    assert_eq!(host.refresh_calls.get(), 1);
}

#[test]
fn relocation_is_one_shot_admission_authority() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_source(&temp);
    let old_path = temp.path().join("old-history.jsonl");
    let mut host = FakeHost::new(
        path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    let lineage = host.lineage_hex();
    host.publication = Some(publication(receipt(
        Some(lineage),
        SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), false),
    )));
    let mut request = exact_request(path);
    request.relocate_from = Some(old_path.clone());

    run_ingest(&request, temp.path(), &mut host).unwrap();

    assert_eq!(host.relocate_from.borrow().as_ref(), Some(&old_path));
    assert_eq!(host.admission_calls.get(), 1);
    assert_eq!(host.refresh_calls.get(), 1);
}

#[test]
fn automatic_failure_rejection_totals_and_bounded_omissions_remain_exact() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = write_source(&temp);
    let mut result = SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true);
    result.source_failure_total = 4;
    result.source_retryable_failure_total = 4;
    result.source_failures = vec![SourceBackedRefreshSourceFailure {
        route_identity: ROUTE.to_owned(),
        source_identity: SOURCE_ID.to_owned(),
        provider: "codex".to_owned(),
        class: "unavailable".to_owned(),
        carried_forward: true,
        source_selector: source_path.display().to_string(),
        detail: "temporarily unavailable".to_owned(),
    }];
    result.rejected_record_total = 5;
    result.rejection_diagnostics = vec![SourceBackedRefreshRecordRejection {
        route_identity: ROUTE.to_owned(),
        source_identity: SOURCE_ID.to_owned(),
        provider: "codex".to_owned(),
        source_selector: source_path.display().to_string(),
        line: 7,
        payload_type: "event".to_owned(),
        class: "malformed_record".to_owned(),
        detail: "bad event".to_owned(),
    }];
    let mut host = FakeHost::new(source_path.clone(), publication(receipt(None, result)));
    host.all_discovery.sources = vec![provider_source(
        source_path,
        ProviderSourceStatus::Available,
    )];

    let report = run_ingest(
        &IngestRequest::default(),
        &temp.path().join("ctx"),
        &mut host,
    )
    .unwrap();

    assert_eq!(report.totals.failed_sources, 4);
    assert_eq!(report.totals.failed, 5);
    assert_eq!(report.totals.sources_completed_with_rejections, 1);
    let IngestSourceOutcome::Automatic(summary) = &report.sources[0] else {
        panic!("expected automatic summary")
    };
    assert_eq!(summary.status, crate::IngestStatus::Partial);
    assert_eq!(summary.source_failures_omitted, 3);
    assert_eq!(summary.rejection_diagnostics_omitted, 4);
    assert_eq!(report.sources.len(), 3);
    assert_eq!(host.refresh_calls.get(), 1);
}

#[test]
fn rejection_only_publication_is_published_but_all_unusable_input_fails() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = write_source(&temp);
    let mut result = SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true);
    result.rejected_record_total = 2;

    let mut usable_receipt = receipt(None, result.clone());
    usable_receipt.current.complete_records = 3;
    usable_receipt.current.retained_records = 1;
    usable_receipt.current.rejected_records = 2;
    let mut usable_host = FakeHost::new(source_path.clone(), publication(usable_receipt));
    usable_host.all_discovery.sources = vec![provider_source(
        source_path.clone(),
        ProviderSourceStatus::Available,
    )];

    let usable = run_ingest(
        &IngestRequest::default(),
        &temp.path().join("ctx-usable"),
        &mut usable_host,
    )
    .unwrap();
    assert_eq!(
        usable.totals.outcome().0,
        crate::ImportOutcome::CompletedWithRejections
    );
    let IngestSourceOutcome::Automatic(usable_source) = &usable.sources[0] else {
        panic!("expected automatic summary")
    };
    assert_eq!(usable_source.status, crate::IngestStatus::Partial);

    let mut unusable_receipt = receipt(None, result);
    unusable_receipt.current.complete_records = 2;
    unusable_receipt.current.retained_records = 0;
    unusable_receipt.current.rejected_records = 2;
    let mut unusable_host = FakeHost::new(source_path.clone(), publication(unusable_receipt));
    unusable_host.all_discovery.sources = vec![provider_source(
        source_path.clone(),
        ProviderSourceStatus::Available,
    )];

    let unusable = run_ingest(
        &IngestRequest::default(),
        &temp.path().join("ctx-unusable"),
        &mut unusable_host,
    )
    .unwrap();
    assert_eq!(unusable.totals.outcome().0, crate::ImportOutcome::Failure);
    let IngestSourceOutcome::Automatic(unusable_source) = &unusable.sources[0] else {
        panic!("expected automatic summary")
    };
    assert_eq!(unusable_source.status, crate::IngestStatus::Partial);

    let mut ignored_receipt = receipt(
        None,
        SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
    );
    ignored_receipt.current.complete_records = 2;
    ignored_receipt.current.retained_records = 0;
    ignored_receipt.current.ignored_records = 2;
    let mut ignored_host = FakeHost::new(source_path.clone(), publication(ignored_receipt));
    ignored_host.all_discovery.sources = vec![provider_source(
        source_path,
        ProviderSourceStatus::Available,
    )];

    let ignored = run_ingest(
        &IngestRequest::default(),
        &temp.path().join("ctx-ignored"),
        &mut ignored_host,
    )
    .unwrap();
    assert_eq!(ignored.totals.outcome().0, crate::ImportOutcome::Failure);
    let IngestSourceOutcome::Automatic(ignored_source) = &ignored.sources[0] else {
        panic!("expected automatic summary")
    };
    assert_eq!(ignored_source.status, crate::IngestStatus::Published);
}

#[test]
fn refresh_cancellation_propagates_without_retry() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = write_source(&temp);
    let mut host = FakeHost::new(
        source_path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    host.all_discovery.sources = vec![provider_source(
        source_path,
        ProviderSourceStatus::Available,
    )];
    host.refresh_error = Some("cancelled by caller");

    let error = run_ingest(
        &IngestRequest::default(),
        &temp.path().join("ctx"),
        &mut host,
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled by caller");
    assert_eq!(host.refresh_calls.get(), 1);
}

#[test]
fn exact_refresh_disappearance_reports_the_original_requested_path() {
    let temp = tempfile::tempdir().unwrap();
    let owned = write_source(&temp);
    let requested = temp.path().join("relative-request.jsonl");
    let mut host = FakeHost::new(
        owned,
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    host.refresh_path_missing = true;

    let error = run_ingest(&exact_request(requested.clone()), temp.path(), &mut host).unwrap_err();
    let diagnostic = error.downcast_ref::<ImportPathNotFound>().unwrap();

    assert_eq!(diagnostic.path(), requested);
    assert_eq!(host.refresh_calls.get(), 1);
    assert!(error.chain().any(|cause| {
        cause.to_string() == "explicit import path disappeared during refresh admission"
    }));
}

#[test]
fn admission_and_progress_errors_propagate_without_publication() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_source(&temp);
    let mut progress_host = FakeHost::new(
        path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    progress_host.progress_error = Some("progress output cancelled");
    let error = run_ingest(
        &exact_request(path.clone()),
        temp.path(),
        &mut progress_host,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "progress output cancelled");
    assert_eq!(progress_host.admission_calls.get(), 0);
    assert_eq!(progress_host.refresh_calls.get(), 0);

    let mut admission_host = FakeHost::new(
        path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    admission_host.admission_error = Some("relocation authority rejected");
    let error = run_ingest(&exact_request(path), temp.path(), &mut admission_host).unwrap_err();
    assert_eq!(error.to_string(), "relocation authority rejected");
    assert_eq!(admission_host.admission_calls.get(), 1);
    assert_eq!(admission_host.refresh_calls.get(), 0);
}

#[test]
fn selected_automatic_provider_uses_only_its_provider_snapshot_and_forwards_selection() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = write_source(&temp);
    let source = provider_source(source_path.clone(), ProviderSourceStatus::Available);
    let mut host = FakeHost::new(
        source_path,
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    host.provider_discovery.sources = vec![source];
    let request = IngestRequest {
        provider: Some(CaptureProvider::Codex),
        ..IngestRequest::default()
    };

    run_ingest(&request, &temp.path().join("ctx"), &mut host).unwrap();

    assert_eq!(host.provider_discovery_calls.get(), 1);
    assert_eq!(host.all_discovery_calls.get(), 0);
    assert_eq!(host.refresh_calls.get(), 1);
    assert_eq!(
        host.refresh_selections.borrow().as_slice(),
        [RefreshSelection::Provider(CaptureProvider::Codex)]
    );
}

#[test]
fn plugin_route_prepares_inventory_once_and_requires_selected_lineage() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = write_source(&temp);
    let manifest = temp.path().join("ctx-history-plugin.json");
    fs::write(
        &manifest,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "name": "example",
            "history_sources": [{
                "id": "default",
                "source_format": "example-v1",
                "path": source_path,
                "enabled": true
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let mut host = FakeHost::new(
        source_path.clone(),
        publication(receipt(
            None,
            SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
        )),
    );
    let lineage = host.lineage_hex();
    host.publication = Some(publication(receipt(
        Some(lineage),
        SourceBackedRefreshRouteResult::succeeded(ROUTE.into(), true),
    )));
    let request = IngestRequest {
        history_source: Some("example/default".to_owned()),
        history_source_manifests: vec![manifest],
        ..IngestRequest::default()
    };

    let report = run_ingest(&request, temp.path(), &mut host).unwrap();

    assert!(matches!(report.sources[0], IngestSourceOutcome::Plugin(_)));
    assert_eq!(host.admission_calls.get(), 1);
    assert_eq!(host.refresh_calls.get(), 1);
    assert_eq!(
        host.events
            .borrow()
            .iter()
            .filter(|event| event.as_str() == "prepare_plugin")
            .count(),
        1
    );
}

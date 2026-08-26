use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
    CapturePublicationContext, CapturePublicationDisposition, CaptureRevalidationTarget,
    CoreMaterialization, CorePreparationFailureKind, CorePreparationPort, ImmutableCaptureSnapshot,
    PresentCaptureRoute, SourceBackedCertifiedRemoval, SourceBackedLogicalSourceFailures,
    SourceBackedRecordRejections, SourceBackedRouteResources,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, SourceKey, TypedKey,
};
use rusqlite::Connection;
use uuid::Uuid;

use super::*;
use crate::provider_sources::{
    SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase, SqliteSourceAccessError,
};

#[derive(Clone, Default)]
pub(crate) struct NoopLookup;

impl BaseEventLookup for NoopLookup {
    type Error = std::io::Error;

    fn contains(&self, _event_id: Uuid) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
pub(crate) struct NoopPreparation;

impl CorePreparationPort for NoopPreparation {
    type Prepared = CoreRecord;
    type Draft = CoreRecord;
    type Failure = std::io::Error;

    fn prepare(&self, record: CoreRecord) -> std::result::Result<Self::Prepared, Self::Failure> {
        Ok(record)
    }

    fn prepare_draft(&self, record: CoreRecord) -> std::result::Result<Self::Draft, Self::Failure> {
        Ok(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        _maximum_encoded_bytes: usize,
    ) -> std::result::Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
        Ok(CoreMaterialization::Prepared(draft))
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        &prepared.source
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared
            .encode_stored()
            .map(|encoded| encoded.len())
            .unwrap_or(0)
    }

    fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
        CorePreparationFailureKind::Internal
    }
}

#[derive(Clone, Default)]
pub(crate) struct NoopSnapshot;

impl ImmutableCaptureSnapshot for NoopSnapshot {
    fn sources(&self) -> &[CertifiedSource] {
        &[]
    }

    fn source_aggregates(
        &self,
    ) -> impl ExactSizeIterator<Item = ctx_history_capture_runtime::CaptureSourceAggregateRef<'_>>
    {
        std::iter::empty()
    }

    fn source_routes(
        &self,
    ) -> impl ExactSizeIterator<Item = ctx_history_capture_runtime::CaptureRouteRef<'_>> {
        std::iter::empty()
    }

    fn source_route(
        &self,
        _route_identity: &SourceRouteIdentity,
    ) -> Option<ctx_history_capture_runtime::CaptureRouteRef<'_>> {
        None
    }
}

#[derive(Default)]
pub(crate) struct NoopLifecycle;

impl CaptureLifecycleSink for NoopLifecycle {
    type Error = std::io::Error;
    type OpenOptions = ();
    type BaseLookup = NoopLookup;
    type Preparation = NoopPreparation;
    type PinnedAppendBase = CertifiedSource;
    type CommittedSnapshot = NoopSnapshot;
    type VerifiedPublication = ();
    type Snapshot<'a> = NoopSnapshot;

    fn invariant_error(detail: &'static str) -> Self::Error {
        std::io::Error::other(detail)
    }

    fn open(
        _root: &std::path::Path,
        _options: Self::OpenOptions,
    ) -> std::result::Result<CaptureLifecycleOpenOutcome<Self>, Self::Error> {
        Ok(CaptureLifecycleOpenOutcome::Ready(Self))
    }

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
        None
    }

    fn base_source(&self, _source: &SourceKey) -> Option<&CertifiedSource> {
        None
    }

    fn pinned_append_base(
        &self,
        _route_identity: &SourceRouteIdentity,
        _source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase> {
        None
    }

    fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource {
        base
    }

    fn base_event_lookup(&self) -> Self::BaseLookup {
        NoopLookup
    }

    fn core_preparation(&self) -> Self::Preparation {
        NoopPreparation
    }

    fn set_route_plan(
        &mut self,
        _selected: BTreeSet<SourceRouteIdentity>,
        _carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn begin_route_stage(
        &mut self,
        _route_identity: SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn retain_unstaged_route_members(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
        false
    }

    fn register_route_revalidation(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _revalidate: impl Fn() -> bool + Send + 'static,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn visit_revalidation_targets<E>(
        &self,
        _visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> std::result::Result<(), E>,
    ) -> std::result::Result<std::result::Result<(), E>, Self::Error> {
        Ok(Ok(()))
    }

    fn finish_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn rollback_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn authorize_carried_route_retirement(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn retire_carried_route(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> std::result::Result<Vec<SourceKey>, Self::Error> {
        Ok(Vec::new())
    }

    fn begin_source_replace(&mut self, _source: SourceKey) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn begin_source_append(
        &mut self,
        _source: SourceKey,
    ) -> std::result::Result<&CertifiedSource, Self::Error> {
        Err(std::io::Error::other("no append base"))
    }

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> std::result::Result<&CertifiedSource, Self::Error> {
        let _ = base;
        Err(std::io::Error::other("no append base"))
    }

    fn add_prepared(
        &mut self,
        _prepared: <Self::Preparation as CorePreparationPort>::Prepared,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn certify_source(
        &mut self,
        _certificate: CertifiedSource,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn certify_source_append(
        &mut self,
        _append: CertifiedSourceAppend,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn retain_source(
        &mut self,
        _certificate: CertifiedSource,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn certify_complete_inventory(
        &mut self,
        _inventory: CertifiedSourceInventory,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn delete_source(
        &mut self,
        _deletion: CertifiedSourceDeletion,
        _inventory: CertifiedSourceInventory,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn carry_failed_route(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }

    fn observe_missing_route(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _observed_at_unix_ms: u64,
        _revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn set_present_routes(
        &mut self,
        _routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn commit<F, I>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
    ) -> std::result::Result<CaptureCommitReceipt<Self::CommittedSnapshot>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(CaptureCommitReceipt::new(
            "noop-generation".to_owned(),
            1,
            0,
            0,
            0,
            NoopSnapshot,
        ))
    }

    fn commit_with_metadata<F, I, M>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
        metadata_factory: M,
    ) -> std::result::Result<
        CaptureCommitOutcome<Self::CommittedSnapshot, Self::VerifiedPublication>,
        Self::Error,
    >
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: for<'a> FnOnce(
            CapturePublicationContext<'a, Self::Snapshot<'a>>,
        ) -> std::result::Result<Vec<u8>, Self::Error>,
    {
        let snapshot = NoopSnapshot;
        let _ = metadata_factory(CapturePublicationContext::new(
            "noop-generation",
            snapshot.clone(),
        ))?;
        Ok(CaptureCommitOutcome::new(
            CaptureCommitReceipt::new("noop-generation".to_owned(), 1, 0, 0, 0, snapshot),
            CapturePublicationDisposition::Published,
            ctx_history_capture_runtime::VerifiedCapture::new(()),
        ))
    }
}

#[derive(Default)]
struct NoopSpool(Vec<CoreRecord>);

impl DocumentRecordSpool for NoopSpool {
    fn new(_resources: SourceBackedRouteResources) -> SourceBackedRouteResult<Self> {
        Ok(Self::default())
    }

    fn push(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
        self.0.push(record);
        Ok(())
    }

    fn replay(
        self,
        mut emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<()> {
        for record in self.0 {
            emit(record)?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct TestCrushInventory {
    observation: CrushProjectInventoryObservationV0,
}

impl CrushProjectInventorySourceV0 for TestCrushInventory {
    fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
        Ok(self.observation.clone())
    }

    fn record_projection_pass(&self) {}

    fn record_snapshot_work(
        &self,
        _work: crate::provider::providers::crush::native_path::source_backed::CrushSnapshotWorkV0,
    ) {
    }
}

fn create_astrbot_database(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "pragma user_version = 4;
                 create table conversations (
                     id integer primary key,
                     inner_conversation_id text,
                     conversation_id text,
                     platform_id text,
                     user_id text,
                     content text not null,
                     title text,
                     persona_id text,
                     token_usage text,
                     created_at integer,
                     updated_at integer
                 );
                 create table platform_message_history (
                     id integer primary key,
                     platform_id text,
                     user_id text,
                     sender_id text,
                     sender_name text,
                     content text,
                     llm_checkpoint_id text,
                     created_at integer
                 );
                 insert into conversations (
                     id, inner_conversation_id, conversation_id, platform_id, user_id, content,
                     title, persona_id, token_usage, created_at, updated_at
                 ) values (
                     1, 'session', 'conversation', 'webchat', 'user',
                     '[{\"id\":\"message\",\"role\":\"user\",\"content\":\"body\"}]',
                     'title', 'persona', '{\"prompt\":1,\"completion\":2}', 1, 1
                 );",
        )
        .unwrap();
}

fn fixture_provider_source(
    provider: CaptureProvider,
    path: PathBuf,
    source_format: &'static str,
) -> ProviderSource {
    ProviderSource {
        provider,
        path,
        exists: true,
        source_format,
        source_kind: ctx_history_capture_model::ProviderSourceKind::NativeHistory,
        import_support: ctx_history_capture_model::ProviderImportSupport::Native,
        catalog_support: ctx_history_capture_model::ProviderCatalogSupport::None,
        status: crate::ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

fn shelley_registration_error(error: SqliteSourceAccessError) -> SourceBackedRouteError {
    shelley_inventory_route_error(
        crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError::SqliteSource(
            error,
        ),
    )
}

#[test]
fn shelley_registration_preserves_corrupt_source_classification() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let exact_cwd = temp.path().join("workspace");
    let database = exact_cwd.join("shelley.db");
    std::fs::create_dir_all(&exact_cwd).unwrap();
    std::fs::write(&database, b"not a sqlite database").unwrap();
    let source = fixture_provider_source(
        CaptureProvider::Shelley,
        database,
        crate::SHELLEY_SQLITE_SOURCE_FORMAT,
    );

    let error = shelley_registration::<NoopLifecycle, NoopSpool>(
        source,
        SourceBackedRouteSelection::Automatic,
        crate::test_provider_sqlite_data_root(),
        exact_cwd,
    )
    .err()
    .expect("corrupt Shelley source must be rejected");

    assert_eq!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
    assert!(error.detail.contains("not a database"));
}

#[test]
fn shelley_registration_preserves_source_change_classification() {
    let error = shelley_registration_error(SqliteSourceAccessError::SourceChanged);

    assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
}

#[test]
fn shelley_registration_treats_disappearance_after_discovery_as_source_change() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let exact_cwd = temp.path().join("workspace");
    let database = exact_cwd.join("shelley.db");
    std::fs::create_dir_all(&exact_cwd).unwrap();
    Connection::open(&database)
        .unwrap()
        .execute_batch("create table admitted_before_registration(value text);")
        .unwrap();
    let source = fixture_provider_source(
        CaptureProvider::Shelley,
        database.clone(),
        crate::SHELLEY_SQLITE_SOURCE_FORMAT,
    );
    std::fs::remove_file(database).unwrap();

    let error = shelley_registration::<NoopLifecycle, NoopSpool>(
        source,
        SourceBackedRouteSelection::Automatic,
        crate::test_provider_sqlite_data_root(),
        exact_cwd,
    )
    .err()
    .expect("disappeared Shelley source must be retried");

    assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
    assert!(error.detail.contains("no longer contains"));
}

#[test]
fn shelley_registration_preserves_resource_unavailable_classification() {
    let error = shelley_registration_error(SqliteSourceAccessError::ResourceUnavailable {
        operation: "test Shelley admission",
        path: PathBuf::from("shelley.db"),
        source: std::io::Error::from(std::io::ErrorKind::OutOfMemory),
    });

    assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
}

#[test]
fn sqlite_contention_is_logical_but_exhaustion_remains_route_fatal() {
    for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
        let diagnosed = |artifact| {
            SqliteSourceAccessError::SqliteControl {
                operation: "querying a contended provider database",
                code,
            }
            .with_diagnostic(
                SqliteFailurePhase::Projection,
                artifact,
                0,
                0,
                SqliteCleanupStatus::NotRequired,
            )
        };
        let provider = diagnosed(SqliteArtifactKind::ProviderDatabase);
        let private = diagnosed(SqliteArtifactKind::PrivateSourceCopy);
        let raw = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);

        assert_eq!(
            sqlite_source_route_error_kind(&provider),
            SourceBackedRouteErrorKind::Unavailable
        );
        assert!(sqlite_source_route_error_kind(&provider).is_logical_source_failure());
        assert_eq!(
            sqlite_source_route_error_kind(&private),
            SourceBackedRouteErrorKind::Internal
        );
        assert_eq!(
            sqlite_capture_route_error(&CaptureError::Sqlite(raw)),
            Some(SourceBackedRouteErrorKind::Internal)
        );
    }

    for code in [rusqlite::ffi::SQLITE_FULL, rusqlite::ffi::SQLITE_NOMEM] {
        let source = SqliteSourceAccessError::SqliteControl {
            operation: "querying an exhausted provider database",
            code,
        };
        let raw = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);

        assert_eq!(
            sqlite_source_route_error_kind(&source),
            SourceBackedRouteErrorKind::ResourceUnavailable
        );
        assert!(!sqlite_source_route_error_kind(&source).is_logical_source_failure());
        assert_eq!(
            sqlite_capture_route_error(&CaptureError::Sqlite(raw)),
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        );
    }
}

#[test]
fn sqlite_inventory_snapshot_capacity_failure_is_route_local() {
    let error = shelley_registration_error(SqliteSourceAccessError::InsufficientScratchSpace {
        path: PathBuf::from("ctx-data"),
        required: 10 * 1024 * 1024 * 1024,
        available: 5 * 1024 * 1024 * 1024,
    });

    assert_eq!(error.kind, SourceBackedRouteErrorKind::Unavailable);
}

#[test]
fn sqlite_inventory_watch_targets_include_databases_and_authority_parents() {
    let first = PathBuf::from("/tmp/a/history.sqlite");
    let second = PathBuf::from("/tmp/b/state.db");
    let targets = sqlite_inventory_watch_targets([first.as_path(), second.as_path()]);
    assert_eq!(targets.sqlite_databases.len(), 2);
    assert!(targets.sqlite_databases.contains(&first));
    assert!(targets.sqlite_databases.contains(&second));
    assert!(targets.authority_paths.contains(&PathBuf::from("/tmp/a")));
    assert!(targets.authority_paths.contains(&PathBuf::from("/tmp/b")));
}

#[test]
fn astrbot_registration_watch_targets_cover_selected_and_launcher_instances() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("workspace");
    let selected = cwd.join("data/data_v4.db");
    let launcher = home.join(
        ".astrbot_launcher/instances/123e4567-e89b-12d3-a456-426614174000/core/data/data_v4.db",
    );
    let ignored = home.join(".astrbot_launcher/instances/not-a-uuid/core/data/data_v4.db");
    create_astrbot_database(&selected);
    create_astrbot_database(&launcher);
    create_astrbot_database(&ignored);

    let registration = astrbot_registration::<NoopLifecycle, NoopSpool>(
        fixture_provider_source(
            CaptureProvider::AstrBot,
            selected.clone(),
            ASTRBOT_SQLITE_SOURCE_FORMAT,
        ),
        SourceBackedRouteSelection::Automatic,
        crate::test_provider_sqlite_data_root(),
        DiscoveryContext::new(
            &home,
            &cwd,
            ctx_history_source_discovery::DiscoveryPlatform::Linux,
            ctx_history_source_discovery::DiscoveryPlatformDirs::default(),
        ),
    );
    let (_, _, _, _, watch_targets) = registration.into_parts();
    let targets = watch_targets.unwrap()().unwrap();
    assert!(targets.sqlite_databases.contains(&selected));
    assert!(targets.sqlite_databases.contains(&launcher));
    assert!(!targets.sqlite_databases.contains(&ignored));
    assert!(targets.authority_paths.contains(selected.parent().unwrap()));
    assert!(targets.authority_paths.contains(launcher.parent().unwrap()));
    assert!(targets
        .authority_paths
        .contains(&home.join(".astrbot_launcher").join("instances")));
}

#[test]
fn crush_registration_watch_targets_follow_observed_inventory() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = temp.path().join("alpha/project.db");
    let second = temp.path().join("beta/project.db");
    let observation = CrushProjectInventoryObservationV0::new(
        TypedKey::utf8("crush-inventory").unwrap(),
        b"revision".to_vec(),
        vec![
            CrushProjectDatabaseV0::new(TypedKey::utf8("alpha").unwrap(), first.clone()).unwrap(),
            CrushProjectDatabaseV0::new(TypedKey::utf8("beta").unwrap(), second.clone()).unwrap(),
        ],
    )
    .unwrap();
    let registration = crush_registration::<TestCrushInventory, NoopLifecycle, NoopSpool>(
        fixture_provider_source(
            CaptureProvider::Crush,
            first.clone(),
            CRUSH_SQLITE_SOURCE_FORMAT,
        ),
        SourceBackedRouteSelection::Automatic,
        crate::test_provider_sqlite_data_root(),
        Arc::new(TestCrushInventory { observation }),
    );
    let (_, _, _, _, watch_targets) = registration.into_parts();
    let targets = watch_targets.unwrap()().unwrap();
    assert!(targets.sqlite_databases.contains(&first));
    assert!(targets.sqlite_databases.contains(&second));
    assert!(targets.authority_paths.contains(first.parent().unwrap()));
    assert!(targets.authority_paths.contains(second.parent().unwrap()));
}

#[test]
fn lingma_registration_watch_targets_follow_inventory_databases() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = temp.path().join("alpha/state.vscdb");
    let second = temp.path().join("beta/state.vscdb");
    let registration = lingma_registration::<NoopLifecycle, NoopSpool>(
        fixture_provider_source(
            CaptureProvider::Lingma,
            first.clone(),
            LINGMA_SQLITE_SOURCE_FORMAT,
        ),
        SourceBackedRouteSelection::Automatic,
        crate::test_provider_sqlite_data_root(),
        TypedKey::utf8("lingma-authority").unwrap(),
        vec![
            (first.clone(), TypedKey::utf8("lineage-alpha").unwrap()),
            (second.clone(), TypedKey::utf8("lineage-beta").unwrap()),
        ],
    )
    .unwrap();
    let (_, _, _, _, watch_targets) = registration.into_parts();
    let targets = watch_targets.unwrap()().unwrap();
    assert!(targets.sqlite_databases.contains(&first));
    assert!(targets.sqlite_databases.contains(&second));
    assert!(targets.authority_paths.contains(first.parent().unwrap()));
    assert!(targets.authority_paths.contains(second.parent().unwrap()));
}

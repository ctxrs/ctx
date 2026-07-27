use crate::tests::native_sqlite::shelley_fixtures::write_shelley_malformed_db;
use crate::tests::support::paths::tempdir;
use crate::{
    import_shelley_sqlite, ProviderImportSupport, ProviderSourceStatus, ShelleySqliteImportOptions,
    SHELLEY_SQLITE_SOURCE_FORMAT,
};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use std::fs;

#[test]
fn native_shelley_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_shelley_malformed_db(&temp);
    let corrupt = temp.path().join("corrupt-shelley.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_shelley_sqlite(
        &malformed,
        &mut store,
        ShelleySqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("Shelley messages table missing required column(s): type"));

    let err = import_shelley_sqlite(&corrupt, &mut store, ShelleySqliteImportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
fn provider_sources_discovers_shelley_default_db() {
    let temp = tempdir();
    let project = temp.path().join("project");
    let db = project.join("shelley.db");
    let helper = temp.path().join("helper.db");
    fs::create_dir_all(&project).unwrap();
    fs::write(&db, b"not inspected by source probe").unwrap();
    fs::write(&helper, b"not an automatic writer root").unwrap();

    let context = crate::DiscoveryContext::new(
        temp.path(),
        &project,
        crate::DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("SHELLEY_DB", helper);
    let sources = crate::discover_provider_sources_for_provider_with_context(
        &context,
        CaptureProvider::Shelley,
    )
    .sources;
    let source = sources
        .iter()
        .find(|source| source.source_format == SHELLEY_SQLITE_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Shelley);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, db);
}

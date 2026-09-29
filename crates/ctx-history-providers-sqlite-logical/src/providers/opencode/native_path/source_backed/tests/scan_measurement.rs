use super::*;

/// Supply an isolated native or authored database; fixture creation is excluded.
/// Measure process CPU externally with one test thread and an optimized build.
#[test]
#[ignore = "requires CTX_TEST_OPENCODE_DATABASE and CTX_TEST_OPENCODE_RECORDS"]
fn opencode_scan_measurement() {
    let database = std::path::PathBuf::from(
        std::env::var_os("CTX_TEST_OPENCODE_DATABASE").expect("disposable database path"),
    );
    let expected: u64 = std::env::var("CTX_TEST_OPENCODE_RECORDS")
        .expect("expected retained record count")
        .parse()
        .unwrap();
    let temp = crate::test_support_paths::tempdir().unwrap();
    let start = std::time::Instant::now();
    let authorized = open_root_authorized_snapshot_retained(temp.path(), &database).unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let observation =
        observe_logical_source(authorized.sqlite_snapshot.connection().unwrap(), dialect).unwrap();
    let admission = start.elapsed();
    let mut documents = 0_u64;
    let scan = scan_pinned_source(
        &database,
        dialect,
        &observation,
        authorized.sqlite_snapshot,
        &mut |output| {
            match output {
                OpenCodeScanOutput::Document(record) => {
                    record.validate_contract().unwrap();
                    documents += 1;
                }
                OpenCodeScanOutput::Rejection(rejection) => {
                    panic!("unexpected rejection: {rejection:?}");
                }
                _ => {}
            }
            Ok(())
        },
    )
    .unwrap();
    scan.terminal_fence.revalidate().unwrap();
    assert_eq!(documents, expected);
    assert_eq!(scan.certificate.counts().indexed_documents, expected);
    assert_eq!(scan.certificate.counts().rejected_records, 0);
    eprintln!(
        "opencode_scan records={documents} admission={admission:?} total={:?} digest={:x?} bounds={:?}",
        start.elapsed(), scan.certificate.content_digest(), scan.bounds,
    );
}

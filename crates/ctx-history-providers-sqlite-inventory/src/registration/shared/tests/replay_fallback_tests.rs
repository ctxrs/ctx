use super::*;

#[test]
fn missing_or_invalid_commit_header_scans_without_persisting_replay_frontier() {
    for mode in ["missing", "uninitialized", "torn"] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let original = temp.path().join("writer/history.sqlite");
        let _writer = active_wal_database(&original, "committed row");
        let database = temp.path().join("provider/history.sqlite");
        fs::create_dir(database.parent().unwrap()).unwrap();
        // A real stock SQLite family, captured while the fixture writer is
        // idle. Omit SHM or supply an uninitialized/torn index. Private recovery
        // can still read committed WAL rows without touching this provider.
        fs::copy(&original, &database).unwrap();
        fs::copy(
            sqlite_component_path(&original, "-wal"),
            sqlite_component_path(&database, "-wal"),
        )
        .unwrap();
        let shm = sqlite_component_path(&database, "-shm");
        if mode != "missing" {
            let mut bytes = fs::read(sqlite_component_path(&original, "-shm")).unwrap();
            if mode == "uninitialized" {
                bytes[..96].fill(0);
            } else {
                bytes[8] ^= 1;
            }
            fs::write(&shm, bytes).unwrap();
        }
        let before_db = fs::read(&database).unwrap();
        let before_wal = fs::read(sqlite_component_path(&database, "-wal")).unwrap();
        let before_shm = fs::read(&shm).ok();
        let provider = TestProvider::production(Arc::new(Mutex::new(vec![TestLeaf {
            source: test_source("fallback"),
            path: database.clone(),
        }])));
        let data = temp.path().join("data");
        let first = run_provider(&data, provider.clone(), 1, Vec::new()).unwrap();
        let base = first.lifecycle.sources();
        assert_eq!(base.len(), 1);
        assert!(base[0].frontier().is_none());
        assert_eq!(
            provider.sorted_outputs()[0].rows,
            [(1, "committed row".into())]
        );
        provider.reset_run();
        let repeated = run_provider(&data, provider.clone(), 1, base).unwrap();
        assert_eq!(
            provider.state.lock().unwrap().projections,
            1,
            "no unsafe exact replay"
        );
        assert!(repeated.lifecycle.sources()[0].frontier().is_none());
        assert_eq!(fs::read(&database).unwrap(), before_db);
        assert_eq!(
            fs::read(sqlite_component_path(&database, "-wal")).unwrap(),
            before_wal
        );
        assert_eq!(fs::read(&shm).ok(), before_shm);
        assert_no_snapshot_temp_leak(&data);
    }
}

#[test]
fn discovered_replay_fingerprint_cannot_certify_a_later_snapshot() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider/history.sqlite");
    let writer = active_wal_database(&database, "old row");
    let data = temp.path().join("data");
    let provider = TestProvider::production(Arc::new(Mutex::new(vec![TestLeaf {
        source: test_source("changed"),
        path: database.clone(),
    }])));
    let adapter = SqliteInventoryDocumentAdapter::new(
        &data,
        CaptureProvider::Shelley,
        "shelley_sqlite",
        provider.clone(),
    );
    let driver = ctx_history_capture_runtime::replacement_document_tree_driver(
        DocumentInventoryAuthority::new(CaptureProvider::Shelley.as_str().to_owned(), [0x31; 32]),
        CommitAfterDiscovery {
            adapter,
            writer: Mutex::new(writer),
        },
    );
    let mut harness = SinkHarness::with_base(Vec::new());
    let error = driver.scan(&mut harness.sink(1)).unwrap_err();
    assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
    assert_eq!(provider.state.lock().unwrap().projections, 0);
    assert!(harness.lifecycle.sources().is_empty());
    assert_no_snapshot_temp_leak(&data);

    let fresh = run_provider(&data, provider.clone(), 1, Vec::new()).unwrap();
    let base = fresh.lifecycle.sources();
    assert!(base[0].frontier().is_some());
    assert_eq!(provider.sorted_outputs()[0].rows, [(1, "new row".into())]);
    provider.reset_run();
    run_provider(&data, provider.clone(), 1, base).unwrap();
    assert_zero_snapshot_replay(&provider, 1);
}

// Drive the real replacement lifecycle, with a committed write exactly after
// the production adapter has advertised its fingerprint and before acquisition.
struct CommitAfterDiscovery {
    adapter: SqliteInventoryDocumentAdapter<TestProvider, TestLifecycle, TestSpool>,
    writer: Mutex<Connection>,
}
impl ReplacementDocumentTree for CommitAfterDiscovery {
    type Lifecycle = TestLifecycle;
    type Spool = TestSpool;
    type RouteControl = crate::ProviderRouteControlExpectation;
    type Leaf = SqliteInventoryDocumentLeaf<TestLeaf>;
    type TreeAuthority = SqliteInventoryTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        self.adapter.parser_revision()
    }
    fn owns_source(&self, source: &SourceKey) -> bool {
        self.adapter.owns_source(source)
    }
    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let tree = self.adapter.discover_complete()?;
        self.writer
            .lock()
            .unwrap()
            .execute("UPDATE messages SET body='new row'", [])
            .unwrap();
        Ok(tree)
    }
    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, Self::Lifecycle, Self::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        self.adapter.scan_changed(authority, leaf, sink)
    }
    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        self.adapter.revalidate_complete(tree)
    }
}

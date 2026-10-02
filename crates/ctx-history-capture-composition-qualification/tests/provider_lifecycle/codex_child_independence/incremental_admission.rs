use super::*;

const OWNER: &str = "019fb000-0000-7000-8000-000000000081";
const PEER: &str = "019fb000-0000-7000-8000-000000000082";

// Authored ownership and I/O adversaries, supplementing native provider fixtures.
#[test]
fn complete_ownerless_records_quarantine_but_empty_and_pending_writes_do_not() {
    for (bytes, failures, records) in [
        (Vec::new(), 0, 0),
        (br#"{"type":"session_meta","payload":"#.to_vec(), 0, 0),
        (jsonl_bytes([message("unowned-message")]), 1, 0),
        (b"malformed complete record\n".to_vec(), 1, 0),
        (
            b"{\"type\":\"session_meta\",\"payload\":{}}\n".to_vec(),
            1,
            0,
        ),
        (
            jsonl_bytes([
                session_meta(PEER, ProviderNativeSessionRelationship::Root, None),
                message("unowned-message"),
            ]),
            1,
            0,
        ),
        (
            jsonl_bytes([
                session_meta(OWNER, ProviderNativeSessionRelationship::Root, None),
                message("owned-message"),
            ]),
            0,
            1,
        ),
    ] {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index_root = temp.path().join("index");
        fs::create_dir(&sessions).unwrap();
        write_session(
            &sessions,
            PEER,
            ProviderNativeSessionRelationship::Root,
            None,
            [message("healthy-neighbor")],
        );
        fs::write(session_path(&sessions, OWNER), &bytes).unwrap();
        let receipt = refresh_source_backed_generation(
            &index_root,
            &register_tree(&[&sessions]),
            writer_options(),
        )
        .unwrap();
        assert!(receipt.failed_routes.is_empty());
        assert_eq!(receipt.logical_source_failures.total(), failures);
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert!(source_records_contain(&index, PEER, "healthy-neighbor"));
        let owner_records = index
            .manifest()
            .sources
            .iter()
            .find(|certificate| {
                source_native_session_id(certificate.observation().source()) == Some(OWNER)
            })
            .map(|_| records_for(&index, OWNER))
            .unwrap_or_default();
        assert_eq!(owner_records.len(), records);
        if failures != 0 {
            assert!(index.manifest().sources.iter().all(|certificate| {
                source_native_session_id(certificate.observation().source()) != Some(OWNER)
            }));
        }
    }
}

#[test]
fn appended_owner_conflict_retains_last_good_across_repeated_imports_and_repairs() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir(&sessions).unwrap();
    write_session(
        &sessions,
        OWNER,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("certified-message")],
    );
    let path = session_path(&sessions, OWNER);
    let good = fs::read(&path).unwrap();
    let cold = refresh_source_backed_generation(
        &index_root,
        &register_tree(&[&sessions]),
        writer_options(),
    )
    .unwrap();
    append_event(&path, message("must-not-publish"));
    append_event(
        &path,
        session_meta(PEER, ProviderNativeSessionRelationship::Root, None),
    );
    let good_sources = cold.sources.clone();
    let mut base = cold;
    for _ in 0..2 {
        let registry = register_tree(&[&sessions]);
        let (failed, _) = incremental_refresh(&index_root, &registry, &base);
        assert!(failed.failed_routes.is_empty());
        assert_eq!(failed.logical_source_failures.total(), 1);
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(index.manifest().sources, good_sources);
        assert!(source_records_contain(&index, OWNER, "certified-message"));
        assert!(search_event_candidates(&index, "must-not-publish", 8).is_empty());
        base = failed;
    }
    fs::write(&path, good).unwrap();
    append_event(&path, message("repaired-message"));
    let (repaired, _) = incremental_refresh(&index_root, &register_tree(&[&sessions]), &base);
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(records_for(&index, OWNER).len(), 2);
    assert!(source_records_contain(&index, OWNER, "repaired-message"));
}

#[test]
fn same_size_owner_rewrite_with_restored_mtime_cannot_reuse_admission() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir(&sessions).unwrap();
    write_session(
        &sessions,
        OWNER,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("certified-message")],
    );
    let path = session_path(&sessions, OWNER);
    let before = fs::metadata(&path).unwrap();
    let cold = refresh_source_backed_generation(
        &index_root,
        &register_tree(&[&sessions]),
        writer_options(),
    )
    .unwrap();
    let bytes = fs::read_to_string(&path).unwrap().replace(OWNER, PEER);
    fs::write(&path, bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
    let after = fs::metadata(&path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    let (failed, _) = incremental_refresh(&index_root, &register_tree(&[&sessions]), &cold);
    assert!(failed.failed_routes.is_empty());
    assert_eq!(failed.logical_source_failures.total(), 1);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(index.manifest().sources, cold.sources);
    assert!(source_records_contain(&index, OWNER, "certified-message"));
}

#[cfg(target_os = "linux")]
fn process_read_bytes() -> u64 {
    fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn unchanged_large_owner_prefix_reuses_checkpoint_in_new_process() {
    const ROOT_ENV: &str = "CTX_TEST_CODEX_PREFIX_ROOT";
    if let Some(root) = std::env::var_os(ROOT_ENV) {
        let root = PathBuf::from(root);
        let sessions = root.join("sessions");
        let index_root = root.join("index");
        let initial = VerifiedIndex::open_pinned(&index_root).unwrap();
        let generation = initial.generation_id().to_owned();
        let sources = initial.manifest().sources.clone();
        drop(initial);
        let registry = register_tree(&[&sessions]);
        let before = process_read_bytes();
        let receipt = io_refresh(&index_root, &registry);
        let read_bytes = process_read_bytes() - before;
        assert!(
            read_bytes < 1024 * 1024,
            "unchanged refresh read {read_bytes} bytes"
        );
        assert!(receipt.failed_routes.is_empty());
        assert!(receipt.logical_source_failures.is_empty());
        assert_eq!(receipt.sources, sources);
        assert_eq!(receipt.commit.generation_id, generation);
        // A watch-selected member must reach the same checkpoint admission.
        let before = process_read_bytes();
        let member = incremental_refresh_member(
            &index_root,
            &registry,
            &receipt,
            &sessions,
            session_path(&sessions, OWNER),
        );
        let read_bytes = process_read_bytes() - before;
        assert!(
            read_bytes < 1024 * 1024,
            "unchanged member read {read_bytes} bytes"
        );
        assert_eq!(member.sources, sources);
        assert_eq!(member.commit.generation_id, generation);
        return;
    }

    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    let mut padding = turn_context();
    padding["payload"]["padding"] = serde_json::json!("x".repeat(128 * 1024));
    write_session(
        &sessions,
        OWNER,
        ProviderNativeSessionRelationship::Root,
        None,
        std::iter::once(message("certified-message")).chain(std::iter::repeat_n(padding, 30)),
    );
    let cold = io_refresh(&temp.path().join("index"), &register_tree(&[&sessions]));
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    assert!(fs::metadata(session_path(&sessions, OWNER)).unwrap().len() > 3 * 1024 * 1024);
    for _ in 0..2 {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "codex_child_independence::incremental_admission::unchanged_large_owner_prefix_reuses_checkpoint_in_new_process",
                "--nocapture",
            ])
            .env(ROOT_ENV, temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[cfg(target_os = "linux")]
fn io_refresh(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
) -> SourceBackedRefreshReceipt {
    // Keep the same publication envelope for exhaustive and member refreshes;
    // changing it would itself create a generation even if every source is unchanged.
    SourceBackedRefreshExecutor::new(registry.clone(), writer_options())
        .refresh_physical_scope_with_detailed_progress_generation_state_reconciliation_and_worksets(
            index_root,
            SourceBackedRefreshScope::All,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Incremental,
            BTreeMap::new(),
            |_| Ok(()),
            |_| GenerationStateEnvelope::new("ctx.test.empty.v1", Vec::new()),
        )
        .unwrap()
}

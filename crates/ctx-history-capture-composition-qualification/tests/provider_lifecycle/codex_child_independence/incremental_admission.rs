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
fn deferred_conflicting_owner_completion_in_member_refresh_retains_prior_records_and_repairs() {
    assert_deferred_owner_member_quarantine_and_repair(session_meta(
        PEER,
        ProviderNativeSessionRelationship::Root,
        None,
    ));
}

#[test]
fn deferred_malformed_owner_completion_in_member_refresh_retains_prior_records_and_repairs() {
    assert_deferred_owner_member_quarantine_and_repair(serde_json::json!({
        "type": "session_meta",
        "payload": {}
    }));
}

fn assert_deferred_owner_member_quarantine_and_repair(metadata: serde_json::Value) {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir(&sessions).unwrap();
    for (owner, marker) in [(OWNER, "certified-message"), (PEER, "healthy-neighbor")] {
        write_session(
            &sessions,
            owner,
            ProviderNativeSessionRelationship::Root,
            None,
            [message(marker)],
        );
    }
    let path = session_path(&sessions, OWNER);
    let good = fs::read(&path).unwrap();
    let cold = refresh_source_backed_generation(
        &index_root,
        &register_tree(&[&sessions]),
        writer_options(),
    )
    .unwrap();
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    let (prior_records, peer_records) = {
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let prior = records_for(&index, OWNER);
        let peer = records_for(&index, PEER);
        assert_eq!(prior.len(), 1);
        assert_eq!(peer.len(), 1);
        (prior, peer)
    };

    let deferred = jsonl_bytes([metadata]);
    let split = deferred.len() / 2;
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&deferred[..split])
        .unwrap();
    let pending = incremental_refresh_member(
        &index_root,
        &register_tree(&[&sessions]),
        &cold,
        &sessions,
        path.clone(),
    );
    assert!(pending.failed_routes.is_empty());
    assert!(pending.logical_source_failures.is_empty());
    assert_ne!(pending.sources, cold.sources);
    {
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(records_for(&index, OWNER), prior_records);
        assert_eq!(records_for(&index, PEER), peer_records);
        assert_eq!(
            certificate_for(&index, OWNER)
                .frontier()
                .unwrap()
                .certified_prefix_bytes(),
            good.len() as u64
        );
    }

    // Completing this deferred record must quarantine the entire candidate
    // before the following otherwise-valid message can be published.
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&deferred[split..])
        .unwrap();
    append_event(&path, message("deferredownerleakmarker"));
    let retained_sources = pending.sources.clone();
    let mut base = pending;
    for _ in 0..2 {
        let failed = incremental_refresh_member(
            &index_root,
            &register_tree(&[&sessions]),
            &base,
            &sessions,
            path.clone(),
        );
        assert!(failed.failed_routes.is_empty());
        assert_eq!(failed.logical_source_failures.total(), 1);
        assert_eq!(failed.sources, retained_sources);
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(index.manifest().sources, retained_sources);
        assert_eq!(records_for(&index, OWNER), prior_records);
        assert_eq!(records_for(&index, PEER), peer_records);
        assert!(search_event_candidates(&index, "deferredownerleakmarker", 8).is_empty());
        base = failed;
    }

    fs::write(&path, good).unwrap();
    append_event(&path, message("deferredownerrepairmarker"));
    let repaired = incremental_refresh_member(
        &index_root,
        &register_tree(&[&sessions]),
        &base,
        &sessions,
        path,
    );
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let repaired_records = records_for(&index, OWNER);
    assert_eq!(repaired_records.len(), 2);
    assert_eq!(repaired_records[0], prior_records[0]);
    assert!(source_records_contain(
        &index,
        OWNER,
        "deferredownerrepairmarker"
    ));
    assert_eq!(records_for(&index, PEER), peer_records);
    assert!(search_event_candidates(&index, "deferredownerleakmarker", 8).is_empty());
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
    io_refresh_with_demand(
        index_root,
        registry,
        SourceBackedReconciliationDemand::Incremental,
    )
}

#[cfg(target_os = "linux")]
fn io_refresh_with_demand(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    demand: SourceBackedReconciliationDemand,
) -> SourceBackedRefreshReceipt {
    io_refresh_with_worksets(index_root, registry, demand, BTreeMap::new())
}

#[cfg(target_os = "linux")]
fn io_refresh_with_worksets(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    demand: SourceBackedReconciliationDemand,
    worksets: BTreeMap<ctx_history_capture_model::SourceRouteIdentity, BTreeSet<PathBuf>>,
) -> SourceBackedRefreshReceipt {
    // Keep the same publication envelope for exhaustive and member refreshes;
    // changing it would itself create a generation even if every source is unchanged.
    SourceBackedRefreshExecutor::new(registry.clone(), writer_options())
        .refresh_physical_scope_with_detailed_progress_generation_state_reconciliation_and_worksets(
            index_root,
            SourceBackedRefreshScope::All,
            SourceBackedRefreshScope::All,
            demand,
            worksets,
            |_| Ok(()),
            |_| GenerationStateEnvelope::new("ctx.test.empty.v1", Vec::new()),
        )
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn appended_large_owner_prefix_reuses_admission_in_new_process() {
    const ROOT_ENV: &str = "CTX_TEST_CODEX_APPEND_PREFIX_ROOT";
    if let Some(root) = std::env::var_os(ROOT_ENV) {
        let root = PathBuf::from(root);
        let sessions = root.join("sessions");
        let index_root = root.join("index");
        let registry = register_tree(&[&sessions]);
        let path = session_path(&sessions, OWNER);
        let member_only = std::env::var_os("CTX_TEST_CODEX_MEMBER_ONLY").is_some();
        let before = process_read_bytes();
        let appended = if member_only {
            io_refresh_with_worksets(
                &index_root,
                &registry,
                SourceBackedReconciliationDemand::Incremental,
                BTreeMap::from([(
                    route_identity(&registry, &sessions),
                    BTreeSet::from([path.clone()]),
                )]),
            )
        } else {
            io_refresh(&index_root, &registry)
        };
        let read_bytes = process_read_bytes() - before;
        assert!(
            read_bytes < 1024 * 1024,
            "first append read {read_bytes} bytes"
        );
        assert!(appended.failed_routes.is_empty());
        assert!(appended.logical_source_failures.is_empty());
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert!(source_records_contain(&index, OWNER, "certified-message"));
        assert!(source_records_contain(&index, OWNER, "append-marker"));
        assert_eq!(records_for(&index, OWNER).len(), 2);
        drop(index);
        let noop = io_refresh(&index_root, &registry);
        assert_eq!(noop.sources, appended.sources);
        assert_eq!(noop.commit.generation_id, appended.commit.generation_id);
        // Same-object rewrite plus growth must still be inspected at an
        // exhaustive boundary; incremental append explicitly trusts old rows.
        let good = io_refresh(&index_root, &registry);
        let rewritten = fs::read_to_string(&path).unwrap().replace(OWNER, PEER);
        fs::write(&path, rewritten).unwrap();
        append_event(&path, message("rewrite-must-not-publish"));
        let rejected = io_refresh_with_demand(
            &index_root,
            &registry,
            SourceBackedReconciliationDemand::Exhaustive,
        );
        assert!(rejected.failed_routes.is_empty());
        assert_eq!(rejected.logical_source_failures.total(), 1);
        assert_eq!(rejected.sources, good.sources);
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert!(search_event_candidates(&index, "rewrite-must-not-publish", 8).is_empty());
        return;
    }

    for member_only in [false, true] {
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
        append_event(&session_path(&sessions, OWNER), message("append-marker"));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "codex_child_independence::incremental_admission::appended_large_owner_prefix_reuses_admission_in_new_process",
                "--nocapture",
            ])
            .env(ROOT_ENV, temp.path())
            .env_remove("CTX_TEST_CODEX_MEMBER_ONLY")
            .envs(member_only.then_some(("CTX_TEST_CODEX_MEMBER_ONLY", "1")))
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
#[test]
fn incomplete_tail_append_resumes_complete_frontier_in_new_process() {
    const ROOT_ENV: &str = "CTX_TEST_CODEX_INCOMPLETE_TAIL_ROOT";
    let pending = jsonl_bytes([message("completed-tail-marker")]);
    let first_end = pending.len() / 3;
    let second_end = pending.len() * 2 / 3;
    if let Some(root) = std::env::var_os(ROOT_ENV) {
        let root = PathBuf::from(root);
        let sessions = root.join("sessions");
        let index_root = root.join("index");
        let registry = register_tree(&[&sessions]);
        let path = session_path(&sessions, OWNER);
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let original_ids = records_for(&index, OWNER)
            .into_iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>();
        assert_eq!(original_ids.len(), 1);
        drop(index);
        let mut base = None;
        for (remaining, complete, member_only) in [
            (&pending[first_end..second_end], false, false),
            (&pending[second_end..], true, true),
        ] {
            if member_only {
                OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(remaining)
                    .unwrap();
            }
            let before = process_read_bytes();
            let advanced = if member_only {
                incremental_refresh_member(
                    &index_root,
                    &registry,
                    base.as_ref().unwrap(),
                    &sessions,
                    path.clone(),
                )
            } else {
                io_refresh(&index_root, &registry)
            };
            let read_bytes = process_read_bytes() - before;
            assert!(
                read_bytes < 1024 * 1024,
                "partial-tail continuation read {read_bytes} bytes"
            );
            assert!(advanced.failed_routes.is_empty());
            assert!(advanced.logical_source_failures.is_empty());
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let records = records_for(&index, OWNER);
            assert_eq!(records.len(), if complete { 2 } else { 1 });
            assert_eq!(records[0].event_id, original_ids[0]);
            assert_eq!(
                source_records_contain(&index, OWNER, "completed-tail-marker"),
                complete
            );
            drop(index);
            let noop = io_refresh(&index_root, &registry);
            assert_eq!(noop.sources, advanced.sources);
            assert_eq!(noop.commit.generation_id, advanced.commit.generation_id);
            base = Some(advanced);
        }
        append_event(&path, message("after-tail-marker"));
        let next = io_refresh(&index_root, &registry);
        assert!(next.failed_routes.is_empty());
        assert!(next.logical_source_failures.is_empty());
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(records_for(&index, OWNER).len(), 3);
        assert!(source_records_contain(&index, OWNER, "after-tail-marker"));
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
    OpenOptions::new()
        .append(true)
        .open(session_path(&sessions, OWNER))
        .unwrap()
        .write_all(&pending[..first_end])
        .unwrap();
    let cold = io_refresh(&temp.path().join("index"), &register_tree(&[&sessions]));
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    OpenOptions::new()
        .append(true)
        .open(session_path(&sessions, OWNER))
        .unwrap()
        .write_all(&pending[first_end..second_end])
        .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "codex_child_independence::incremental_admission::incomplete_tail_append_resumes_complete_frontier_in_new_process",
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

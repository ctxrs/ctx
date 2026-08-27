use super::*;

fn write_terminal_logical_fixture(root: &Path, name: &str, physical: &[u8], logical_eof: u64) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join(format!("{name}.jsonl")), physical).unwrap();
    fs::write(
        root.join(format!("{name}.jsonl.eof")),
        format!("{logical_eof}\n"),
    )
    .unwrap();
}

#[test]
fn exact_present_dependency_rehash_rejects_metadata_equivalent_content_change() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let record = b"{\"message\":\"one\"}\n";
    write_terminal_logical_fixture(&root, "events", record, record.len() as u64);
    let adapter = LogicalEofTestAdapter::default();
    let inventory = adapter.discover(&root).unwrap();
    let mut leaf = inventory.accepted_leaves().next().unwrap().clone();
    let control_path = root.join("events.jsonl.eof");
    let original_modified = fs::metadata(&control_path).unwrap().modified().unwrap();
    let original = fs::read(&control_path).unwrap();
    let mut changed = original.clone();
    changed[0] = if changed[0] == b'1' { b'2' } else { b'1' };
    fs::write(&control_path, &changed).unwrap();
    OpenOptions::new()
        .write(true)
        .open(&control_path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .unwrap();

    // Simulate a platform where the same-object metadata observation is
    // indistinguishable after restoration. Exact content evidence must still
    // reject the dependency.
    let opened = leaf
        .authority()
        .open_file(Path::new("events.jsonl.eof"))
        .unwrap();
    leaf.terminal_dependencies.present[0].observation =
        observe_opened_file(&control_path, &opened).unwrap();
    let error = leaf.terminal_dependencies.present[0]
        .revalidate()
        .expect_err("exact dependency content mutation must fail");
    assert!(error.is_source_changed());
}

#[test]
fn leaf_terminal_sandwich_rejects_control_change_and_absence_creation() {
    for race in ["control", "absence"] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let index = temp.path().join("index");
        let record = b"{\"message\":\"one\"}\n";
        write_terminal_logical_fixture(&root, "events", record, record.len() as u64);
        let adapter = LogicalEofTestAdapter::default();
        let control = root.join("events.jsonl.eof");
        let absent = root.join("events.jsonl.next");
        let hook_ran = Arc::new(AtomicBool::new(false));
        let hook_observation = Arc::clone(&hook_ran);
        set_before_jsonl_terminal_physical_revalidation_hook(root.clone(), move || {
            match race {
                "control" => {
                    let mut changed = fs::read(&control).unwrap();
                    changed[0] = if changed[0] == b'1' { b'2' } else { b'1' };
                    fs::write(&control, changed).unwrap();
                }
                "absence" => fs::write(&absent, b"appeared").unwrap(),
                _ => unreachable!(),
            }
            hook_observation.store(true, Ordering::SeqCst);
        });

        let error =
            capture_parallel_test_generation_with_terminal_revalidation(&adapter, &root, &index, 1)
                .unwrap_err();
        assert!(hook_ran.load(Ordering::SeqCst), "{race}");
        assert!(error.is_source_changed(), "{race}: {error:?}");
    }
}

#[test]
fn leaf_terminal_dependencies_do_not_invalidate_unrelated_leaves() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    let record = b"{\"message\":\"one\"}\n";
    write_terminal_logical_fixture(&root, "alpha", record, record.len() as u64);
    write_terminal_logical_fixture(&root, "beta", record, record.len() as u64);
    let adapter = LogicalEofTestAdapter::default();
    let (_writer, resident, ()) =
        capture_test_generation!(&adapter, &root, &index, 1, |resident, sink| {
            capture(&adapter, &root, resident, sink).unwrap()
        });
    let (alpha, beta, inventory) = {
        let resident = resident.lock().unwrap();
        let opening = resident.opening_inventory.as_ref().unwrap();
        let source_for = |name: &str| {
            let leaf = opening
                .accepted_leaves()
                .find(|leaf| leaf.source_path().ends_with(name))
                .unwrap();
            resident
                .terminal_sources
                .get(&leaf.source().exact_descriptor_digest())
                .unwrap()
                .certificate
                .clone()
        };
        (
            source_for("alpha.jsonl"),
            source_for("beta.jsonl"),
            resident.certified_inventory.clone().unwrap(),
        )
    };
    let alpha_control = root.join("alpha.jsonl.eof");
    let mut changed = fs::read(&alpha_control).unwrap();
    changed[0] = if changed[0] == b'1' { b'2' } else { b'1' };
    fs::write(alpha_control, changed).unwrap();

    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&beta),
    ));
    assert!(!revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&alpha),
    ));
    assert!(
        revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap(),
        "one leaf's control mismatch must not become a route-wide dependency failure"
    );
}

fn seed_sibling_route(index_root: &Path, source: CertifiedSource) {
    let route_source = source.observation().source().clone();
    test_generations().lock().unwrap().insert(
        index_root.to_path_buf(),
        TestSnapshot {
            sources: vec![source],
            route_identity: Some(sibling_route_identity()),
            route_sources: vec![route_source],
            records: Vec::new(),
        },
    );
}

struct TestRouteCapture {
    writer: TestLifecycle,
    resident: Mutex<FamilyResident>,
    owners: HashMap<[u8; 32], SourceOwner>,
    applied_removals: Vec<SourceBackedCertifiedRemoval>,
    logical_source_failures: SourceBackedLogicalSourceFailures,
}

fn capture_current_test_route(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
) -> TestRouteCapture {
    capture_current_test_route_with_owners(adapter, root, index_root, HashMap::new())
}

fn capture_current_test_route_with_sibling_owner(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    sibling_source: SourceKey,
) -> TestRouteCapture {
    let mut owners = HashMap::new();
    owners.insert(
        sibling_source.identity().digest(),
        SourceOwner::new(1, sibling_source, true, None),
    );
    capture_current_test_route_with_owners(adapter, root, index_root, owners)
}

fn capture_current_test_route_with_owners(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    mut owners: HashMap<[u8; 32], SourceOwner>,
) -> TestRouteCapture {
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = match IndexCaptureLifecycle::open(index_root, ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
            panic!("test lifecycle unexpectedly requires recovery")
        }
    };
    let mut complete_inventories = Vec::new();
    let mut applied_removals = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    {
        let mut sink = SourceBackedGenerationSink::new(
            &mut writer,
            &mut owners,
            &mut complete_inventories,
            &mut applied_removals,
            0,
            test_route_identity(),
            None,
            SourceBackedRouteResources::production(1),
            &mut logical_source_failures,
            &mut record_rejections,
            None,
            None,
            None,
        );
        with_family_scanner_workers(1, || capture(adapter, root, &resident, &mut sink)).unwrap();
    }
    TestRouteCapture {
        writer,
        resident,
        owners,
        applied_removals,
        logical_source_failures,
    }
}

#[test]
fn sibling_route_source_is_not_reused_or_claimed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("sibling.jsonl"), TEST_RECORD).unwrap();
    let (sibling_resident, _) = expected_state(&ParallelTestAdapter, &root);
    seed_sibling_route(&index_root, expected_source(&sibling_resident));
    fs::write(root.join("sibling.jsonl"), br#"{"message":"incomplete""#).unwrap();

    let captured = capture_current_test_route(&ParallelTestAdapter, &root, &index_root);

    assert_eq!(jsonl_family_admission_activity().bases, 0);
    assert_eq!(jsonl_family_admission_activity().selected_leaves, 0);
    assert_eq!(captured.writer.activity(), TestLifecycleActivity::default());
    assert!(captured.owners.is_empty());
    assert!(captured.applied_removals.is_empty());
    assert!(captured.logical_source_failures.is_empty());
    assert!(captured.resident.lock().unwrap().owned_sources.is_empty());
}

#[test]
fn sibling_owned_pending_member_does_not_block_current_route_deletion() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    let current_path = root.join("current.jsonl");
    fs::write(&current_path, TEST_RECORD).unwrap();
    let current = capture_parallel_test_generation(&ParallelTestAdapter, &root, &index_root, 1)
        .0
        .manifest
        .sources[0]
        .clone();
    fs::remove_file(current_path).unwrap();
    let sibling_path = root.join("sibling.jsonl");
    fs::write(&sibling_path, br#"{"message":"incomplete""#).unwrap();
    let sibling_source = TestAdapter
        .discover(&root)
        .unwrap()
        .accepted_leaves()
        .next()
        .unwrap()
        .source()
        .clone();

    let captured = capture_current_test_route_with_sibling_owner(
        &ParallelTestAdapter,
        &root,
        &index_root,
        sibling_source,
    );

    assert_eq!(captured.writer.activity().deleted_sources, 1);
    assert_eq!(captured.applied_removals.len(), 1);
    assert!(captured.applied_removals[0]
        .deletion
        .source()
        .exact_descriptor_eq(current.observation().source()));
    assert!(captured.logical_source_failures.is_empty());
}

#[test]
fn sibling_owned_quarantine_does_not_block_current_route_replacement_retirement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    let current_path = root.join("current.jsonl");
    fs::write(&current_path, TEST_RECORD).unwrap();
    let current = capture_parallel_test_generation(&ParallelTestAdapter, &root, &index_root, 1)
        .0
        .manifest
        .sources[0]
        .clone();
    fs::write(root.join("sibling.jsonl"), TEST_RECORD).unwrap();
    let sibling_source = TestAdapter
        .discover(&root)
        .unwrap()
        .accepted_leaves()
        .find(|leaf| leaf.source_path().ends_with("sibling.jsonl"))
        .unwrap()
        .source()
        .clone();

    let captured = capture_current_test_route_with_sibling_owner(
        &ReplacementWithQuarantineTestAdapter,
        &root,
        &index_root,
        sibling_source,
    );

    assert_eq!(captured.writer.activity().begin_source_replacements, 1);
    assert_eq!(captured.writer.activity().deleted_sources, 1);
    assert_eq!(captured.applied_removals.len(), 1);
    assert!(captured.applied_removals[0]
        .deletion
        .source()
        .exact_descriptor_eq(current.observation().source()));
    assert!(captured.logical_source_failures.is_empty());
    assert_eq!(captured.writer.certified_sources.len(), 1);
    assert!(!captured.writer.certified_sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(current.observation().source()));
}

#[test]
fn sibling_route_source_is_not_retired_when_absent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    let sibling_path = root.join("sibling.jsonl");
    fs::write(&sibling_path, TEST_RECORD).unwrap();
    let (sibling_resident, _) = expected_state(&ParallelTestAdapter, &root);
    seed_sibling_route(&index_root, expected_source(&sibling_resident));
    fs::remove_file(sibling_path).unwrap();

    let captured = capture_current_test_route(&ParallelTestAdapter, &root, &index_root);

    assert_eq!(jsonl_family_admission_activity().bases, 0);
    assert_eq!(captured.writer.activity().deleted_sources, 0);
    assert!(captured.owners.is_empty());
    assert!(captured.applied_removals.is_empty());
    assert!(captured.logical_source_failures.is_empty());
}

#[test]
fn sibling_route_source_is_not_quarantined() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("sibling.jsonl"), TEST_RECORD).unwrap();
    let (sibling_resident, _) = expected_state(&ParallelTestAdapter, &root);
    seed_sibling_route(&index_root, expected_source(&sibling_resident));

    let captured = capture_current_test_route(&QuarantinedTestAdapter, &root, &index_root);

    assert_eq!(captured.writer.activity(), TestLifecycleActivity::default());
    assert!(captured.owners.is_empty());
    assert!(captured.applied_removals.is_empty());
    assert!(captured.logical_source_failures.is_empty());
    assert!(captured
        .resident
        .lock()
        .unwrap()
        .quarantined_sources
        .is_empty());
}

#[test]
fn serial_and_parallel_jsonl_emission_preserve_resource_unavailable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for workers in [1, 4] {
        let root = temp.path().join(format!("sessions-{workers}"));
        fs::create_dir_all(&root).unwrap();
        for index in 0..workers {
            fs::write(
                root.join(format!("{index}.jsonl")),
                b"{\"message\":\"bounded\"}\n",
            )
            .unwrap();
        }
        let resident = Mutex::new(FamilyResident::default());
        let mut writer =
            match IndexCaptureLifecycle::open(&temp.path().join(format!("index-{workers}")), ())
                .unwrap()
            {
                CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
                CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
                    panic!("test lifecycle unexpectedly requires recovery")
                }
            };
        let mut owners = HashMap::new();
        let mut complete_inventories = Vec::new();
        let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
        let mut record_rejections = SourceBackedRecordRejections::default();
        let mut applied_removals = Vec::new();
        let mut sink = SourceBackedGenerationSink::new(
            &mut writer,
            &mut owners,
            &mut complete_inventories,
            &mut applied_removals,
            0,
            test_route_identity(),
            None,
            SourceBackedRouteResources::for_test(workers, 1, u64::MAX),
            &mut logical_source_failures,
            &mut record_rejections,
            None,
            None,
            None,
        );

        let error = with_family_scanner_workers(workers, || {
            capture(
                &EmissionTestAdapter::ordinary(),
                &root,
                &resident,
                &mut sink,
            )
            .unwrap_err()
        });
        assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    }
}

#[test]
fn jsonl_terminal_drift_and_io_failures_keep_distinct_route_kinds() {
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::SourceChangedDuringCapture),
        Some(SourceBackedRouteErrorKind::SourceChanged)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::Io(std::io::Error::from_raw_os_error(5))),
        Some(SourceBackedRouteErrorKind::ResourceUnavailable)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::Io(std::io::Error::from_raw_os_error(24))),
        Some(SourceBackedRouteErrorKind::ResourceUnavailable)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::SystemInvariant("broken route")),
        Some(SourceBackedRouteErrorKind::Internal)
    );
    assert_eq!(
        normalized_jsonl_error_kind(&CaptureError::SystemInvariant("broken worker")),
        Some(SourceBackedRouteErrorKind::Internal)
    );
    assert_eq!(
        route_scan(
            &TestAdapter,
            CaptureError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)),
        )
        .kind,
        SourceBackedRouteErrorKind::SourceChanged
    );
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_observes_live_tree() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, b"{\"message\":\"before\"}\n").unwrap();
    let adapter = TestAdapter;

    let (resident, _inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(&first, b"{\"message\":\"changed between callbacks\"}\n").unwrap();
    assert!(
        !revalidate_target(&resident, SourceBackedRevalidationTarget::Source(&source),),
        "the leaf-scoped publication callback must reject the changed source"
    );

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(root.join("new.jsonl"), b"{\"message\":\"late leaf\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );
}

#[cfg(unix)]
#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rejects_admitted_leaf_symlink_race() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let selected = root.join("first.jsonl");
    fs::write(&selected, TEST_RECORD).unwrap();
    let outside = temp.path().join("outside.jsonl");
    fs::write(&outside, b"{\"message\":\"outside must not be read\"}\n").unwrap();
    let adapter = TerminalLeafSwapTestAdapter {
        selected,
        outside,
        enabled: AtomicBool::new(false),
        swapped: AtomicBool::new(false),
    };
    let (resident, inventory) = expected_state(&adapter, &root);
    let resident = Mutex::new(resident);
    adapter.enabled.store(true, Ordering::SeqCst);
    assert!(revalidate_test_sources(&root, &resident).unwrap());

    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap(),
        "an admitted transcript that becomes a symlink must fail terminal membership"
    );
    assert!(adapter.swapped.load(Ordering::SeqCst));
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_accepts_proven_append() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, TEST_RECORD).unwrap();
    let adapter = TestAdapter;
    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));

    OpenOptions::new()
        .append(true)
        .open(&first)
        .unwrap()
        .write_all(b"{\"message\":\"next refresh\"}\n")
        .unwrap();
    assert!(revalidate_complete_inventory(&adapter, &root, &resident, &inventory,).unwrap());
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rejects_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("retained.jsonl"), b"{\"message\":\"kept\"}\n").unwrap();
    let deleted_path = root.join("deleted.jsonl");
    fs::write(&deleted_path, b"{\"message\":\"old\"}\n").unwrap();
    let adapter = TestAdapter;
    let before = adapter.discover(&root).unwrap();
    let deleted_source = before
        .accepted_leaves()
        .find(|leaf| leaf.source_path() == deleted_path)
        .unwrap()
        .source()
        .clone();

    fs::remove_file(&deleted_path).unwrap();
    let (mut resident, inventory) = expected_state(&adapter, &root);
    let opening = resident.opening_inventory.as_ref().unwrap().clone();
    resident
        .absent_sources
        .push(JsonlFamilyAbsentMember::from_path(&opening, deleted_path.clone()).unwrap());
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, b"{\"message\":\"reappeared\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );
}

#[test]
fn same_path_retirement_requires_owned_leaf_and_exact_terminal_source_evidence() {
    fn source(format: &str, key: &str) -> SourceKey {
        SourceKey::derive_provider_native(
            CaptureProvider::Pi.as_str(),
            format,
            TEST_SCHEMA,
            1,
            "terminal-witness-file",
            TypedKey::utf8(key).unwrap(),
        )
        .unwrap()
    }

    fn evidence_for_source(
        evidence: &TerminalSourceEvidence,
        source: SourceKey,
    ) -> TerminalSourceEvidence {
        let admitted = evidence.observed_certificate();
        let observation = ctx_history_core::SourceObservation::new(
            source,
            admitted.observation().revision_kind(),
            admitted.observation().revision().to_vec(),
        )
        .unwrap();
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            admitted.parser_revision(),
            *admitted.content_digest(),
            admitted.counts(),
        )
        .unwrap();
        let mut replacement = evidence.clone();
        replacement.terminal_certificate = Some(certificate);
        replacement
    }

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("recording.jsonl");
    fs::write(&path, TEST_RECORD).unwrap();
    let adapter = TestAdapter;
    let (resident, inventory) = expected_state(&adapter, &root);
    let opening = resident.opening_inventory.as_ref().unwrap();
    let replacement_leaf = opening.accepted_leaves().next().unwrap();
    let replacement_evidence = resident
        .terminal_sources
        .get(&replacement_leaf.source().exact_descriptor_digest())
        .unwrap();
    let old_source = source(TEST_SOURCE_FORMAT, "retired-recording");

    assert!(retirement_absence_dependency(
        &adapter,
        opening,
        &inventory,
        &resident.terminal_sources,
        &old_source,
        &path,
    )
    .is_none());

    // An evidence entry under the expected digest is not sufficient when its
    // admitted certificate describes another source.
    let other_source = source(TEST_SOURCE_FORMAT, "different-recording");
    let mut mismatched_evidence = HashMap::new();
    mismatched_evidence.insert(
        replacement_leaf.source().exact_descriptor_digest(),
        evidence_for_source(replacement_evidence, other_source),
    );
    let mismatched_absence = retirement_absence_dependency(
        &adapter,
        opening,
        &inventory,
        &mismatched_evidence,
        &old_source,
        &path,
    )
    .expect("different-source evidence must not suppress the absence fence");
    assert!(
        !mismatched_absence.remains_absent().unwrap(),
        "the live replacement path must prevent retirement"
    );

    // Even internally consistent terminal evidence cannot authorize an
    // adapter to retire a source in favor of an out-of-family leaf.
    let foreign_source = source("foreign-terminal-witness-jsonl", "foreign-recording");
    let mut foreign_opening = opening.clone();
    let foreign_leaf = foreign_opening
        .members
        .iter_mut()
        .find_map(|member| match member {
            JsonlFamilyInventoryMember::Accepted { leaf, .. } => Some(leaf),
            JsonlFamilyInventoryMember::Quarantined { .. }
            | JsonlFamilyInventoryMember::Pending { .. } => None,
        })
        .unwrap();
    foreign_leaf.source = foreign_source.clone();
    foreign_opening.rebuild_observation().unwrap();
    let foreign_inventory = foreign_opening
        .certify_selected_against(&foreign_opening, vec![foreign_source.clone()])
        .unwrap();
    let mut foreign_evidence = HashMap::new();
    foreign_evidence.insert(
        foreign_source.exact_descriptor_digest(),
        evidence_for_source(replacement_evidence, foreign_source),
    );
    let foreign_absence = retirement_absence_dependency(
        &adapter,
        &foreign_opening,
        &foreign_inventory,
        &foreign_evidence,
        &old_source,
        &path,
    )
    .expect("out-of-family evidence must not suppress the absence fence");
    assert!(
        !foreign_absence.remains_absent().unwrap(),
        "the live out-of-family path must prevent retirement"
    );

    let foreign_old_source = source("foreign-terminal-witness-jsonl", "retired-recording");
    assert!(retirement_absence_dependency(
        &adapter,
        opening,
        &inventory,
        &resident.terminal_sources,
        &foreign_old_source,
        &path,
    )
    .is_some());
}

#[test]
fn active_source_family_contract_jsonl_frozen_multi_root_defers_new_leaves() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_root = temp.path().join("sessions");
    let second_root = temp.path().join("archived_sessions");
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&second_root).unwrap();
    let retained = first_root.join("first.jsonl");
    fs::write(&retained, TEST_RECORD).unwrap();
    fs::write(second_root.join("archived.jsonl"), TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![first_root.clone(), second_root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");

    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);
    fs::write(second_root.join("late.jsonl"), TEST_RECORD).unwrap();
    assert!(revalidate_test_sources(&selection_root, &resident).unwrap());
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );

    let (resident, _inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);
    fs::remove_file(retained).unwrap();
    assert!(
        !revalidate_test_sources(&selection_root, &resident).unwrap_or(false),
        "a deleted leaf must fail its own terminal callback"
    );
}

#[test]
fn exact_route_moved_root_replacement_retains_prior_base_without_old_root_absence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_root = temp.path().join("first-root");
    let replacement_root = temp.path().join("replacement-root");
    let selection_root = temp.path().join("selection");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&replacement_root).unwrap();
    let old_path = first_root.join("retired.jsonl");
    fs::write(&old_path, TEST_RECORD).unwrap();
    fs::write(replacement_root.join("active.jsonl"), TEST_RECORD).unwrap();

    let initial_adapter = FrozenMultiRootTestAdapter {
        roots: vec![first_root],
    };
    let (resident, _inventory) = expected_state(&initial_adapter, &selection_root);
    let prior = expected_source(&resident);
    test_generations().lock().unwrap().insert(
        index_root.clone(),
        TestSnapshot {
            sources: vec![prior.clone()],
            route_identity: Some(test_route_identity()),
            route_sources: vec![prior.observation().source().clone()],
            records: Vec::new(),
        },
    );

    let replacement_adapter = FrozenMultiRootTestAdapter {
        roots: vec![replacement_root],
    };
    let (_writer, _resident, bases) = capture_test_generation!(
        &replacement_adapter,
        &selection_root,
        &index_root,
        1,
        |_resident, sink| { base_sources_for_route(&replacement_adapter, sink) }
    );

    let bases = bases.unwrap();
    assert_eq!(bases.len(), 1);
    assert_eq!(bases[0], prior);

    let (replacement_resident, replacement_inventory) =
        expected_state(&replacement_adapter, &selection_root);
    let replacement_opening = replacement_resident.opening_inventory.as_ref().unwrap();
    assert!(
        old_path.exists(),
        "the old root remains present during replacement"
    );
    assert!(retirement_absence_dependency(
        &replacement_adapter,
        replacement_opening,
        &replacement_inventory,
        &replacement_resident.terminal_sources,
        prior.observation().source(),
        &old_path,
    )
    .is_none());
}

#[test]
fn active_source_family_contract_jsonl_frozen_root_replacement_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    let moved = temp.path().join("moved-sessions");
    fs::rename(&root, &moved).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let sources_revalidated = revalidate_test_sources(&selection_root, &resident);
    assert!(
        sources_revalidated.is_err()
            || revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,)
                .is_err()
    );
}

#[test]
fn active_source_family_contract_jsonl_terminal_noop_is_metadata_only_without_recataloging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = TerminalRootSwapTestAdapter {
        root,
        discoveries: AtomicUsize::new(0),
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    reset_jsonl_prefix_hash_bytes();
    assert!(revalidate_test_sources(&selection_root, &resident).unwrap());
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory).unwrap()
    );
    assert_eq!(adapter.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(jsonl_prefix_hash_bytes(), 0);
}

#[test]
fn active_source_family_contract_jsonl_frozen_rejects_root_swap_without_recataloging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("first.jsonl"), TEST_RECORD).unwrap();
    let adapter = TerminalRootSwapTestAdapter {
        root: root.clone(),
        discoveries: AtomicUsize::new(0),
    };
    let selection_root = temp.path().join("codex-selection");
    let (resident, _inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);

    fs::OpenOptions::new()
        .append(true)
        .open(root.join("first.jsonl"))
        .unwrap()
        .write_all(b"{\"message\":\"appended\"}\n")
        .unwrap();
    let moved = temp.path().join("moved-sessions");
    let swap_root = root.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        fs::rename(&swap_root, moved).unwrap();
        fs::create_dir(&swap_root).unwrap();
        fs::write(swap_root.join("first.jsonl"), TEST_RECORD).unwrap();
        worker_barrier.wait();
    });
    set_after_jsonl_prefix_hash_hook(move || {
        barrier.wait();
        barrier.wait();
    });

    assert!(revalidate_test_sources(&selection_root, &resident).is_err());
    worker.join().unwrap();
    assert_eq!(adapter.discoveries.load(Ordering::SeqCst), 1);
}

#[test]
fn active_source_family_contract_jsonl_frozen_inventory_rejects_deleted_source_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("retained.jsonl"), TEST_RECORD).unwrap();
    let deleted_path = root.join("deleted.jsonl");
    fs::write(&deleted_path, TEST_RECORD).unwrap();
    let adapter = FrozenMultiRootTestAdapter {
        roots: vec![root.clone()],
    };
    let selection_root = temp.path().join("codex-selection");
    let before = adapter.discover(&selection_root).unwrap();
    let deleted_source = before
        .accepted_leaves()
        .find(|leaf| leaf.source_path() == deleted_path)
        .unwrap()
        .source()
        .clone();

    fs::remove_file(&deleted_path).unwrap();
    let (mut resident, inventory) = expected_state(&adapter, &selection_root);
    let opening = resident.opening_inventory.as_ref().unwrap().clone();
    resident
        .absent_sources
        .push(JsonlFamilyAbsentMember::from_path(&opening, deleted_path.clone()).unwrap());
    resident.owned_sources.insert(
        deleted_source.exact_descriptor_digest(),
        deleted_source.clone(),
    );
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, TEST_RECORD).unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );
}

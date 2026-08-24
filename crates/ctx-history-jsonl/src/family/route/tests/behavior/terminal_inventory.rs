use super::*;

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

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(&first, b"{\"message\":\"changed between callbacks\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
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
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).unwrap()
    );

    let (resident, inventory) = expected_state(&adapter, &selection_root);
    let resident = Mutex::new(resident);
    fs::remove_file(retained).unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,)
            .unwrap_or(false)
    );
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
    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).is_err()
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
    let (resident, inventory) = expected_state(&adapter, &selection_root);
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

    assert!(
        revalidate_complete_inventory(&adapter, &selection_root, &resident, &inventory,).is_err()
    );
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

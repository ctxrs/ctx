use super::*;
use crate::protocol::MaterializedCoverage;

#[test]
fn progress_counts_accepted_work_and_retains_final_counters() {
    use crate::materializer::MaterializationPhase;
    use std::sync::Mutex;
    let fixture = Fixture::new();
    let source = source("progress");
    for (revision, records, expected_changes) in [
        (
            1,
            vec![(1, "first".to_owned()), (2, "second".to_owned())],
            2,
        ),
        (
            2,
            vec![
                (1, "first".to_owned()),
                (2, "second".to_owned()),
                (3, "third".to_owned()),
            ],
            1,
        ),
    ] {
        let generation = fixture.publish(revision, &[(source.clone(), records)], None);
        let snapshot =
            crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
        let updates = Mutex::new(Vec::new());
        crate::catch_up_with_progress(&fixture.data_root, &snapshot, &|| false, &|progress| {
            updates.lock().unwrap().push(progress.clone());
            Ok(())
        })
        .unwrap();
        let updates = updates.into_inner().unwrap();
        let complete = updates.last().unwrap();
        assert_eq!(complete.phase, MaterializationPhase::Complete);
        assert_eq!(
            complete.core_generation_id.as_deref(),
            Some(generation.as_str())
        );
        assert_eq!(complete.completed_sources, Some(1));
        assert_eq!(complete.total_sources, Some(1));
        assert_eq!(complete.applied_changes, Some(expected_changes));
        assert!(
            crate::materialization_progress(&fixture.data_root)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::readiness(&fixture.data_root).unwrap().currentness,
            CoreProjectionCurrentness::Current
        );
        crate::catch_up_with_progress(&fixture.data_root, &snapshot, &|| false, &|progress| {
            if progress.phase == MaterializationPhase::Complete {
                assert_eq!(progress.applied_changes, None);
            }
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn progress_observer_failure_cancels_wait_without_modifying_writer() {
    use crate::materializer::MaterializationPhase;
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    let mut owner = fixture.materializer();
    let error =
        crate::catch_up_with_progress(&fixture.data_root, &snapshot, &|| false, &|progress| {
            assert_eq!(progress.phase, MaterializationPhase::WaitingForWriter);
            anyhow::bail!("authored observer failure")
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "authored observer failure");
    assert_eq!(
        sync(&fixture, &generation, &mut owner).core_generation_id,
        generation
    );
}

#[test]
fn read_only_absence_empty_and_abstained_are_honest_terminal_states() {
    let fixture = Fixture::new();
    let legacy = fixture.data_root.join("pro-graph");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("sentinel"), b"untouched legacy bytes").unwrap();
    let absent = crate::readiness(&fixture.data_root).unwrap();
    assert_eq!(
        absent.currentness,
        CoreProjectionCurrentness::NotMaterialized
    );
    let encoded = serde_json::to_value(&absent).unwrap();
    assert_eq!(encoded["currentness"], "not_materialized");
    assert_eq!(
        encoded["diagnostic"]["next_action"]["argv"],
        serde_json::json!(["ctx", "import", "--all"])
    );
    assert!(!fixture.graph_root.exists());

    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    assert!(matches!(
        crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap(),
        CoreMaterializationSyncOutcome::Finished { did_work: true, .. }
    ));
    let empty = crate::readiness(&fixture.data_root).unwrap();
    assert_eq!(empty.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(empty.materialized_coverage, MaterializedCoverage::Empty);
    assert!(empty.diagnostic.is_none());
    assert!(matches!(
        crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap(),
        CoreMaterializationSyncOutcome::Finished {
            did_work: false,
            ..
        }
    ));

    let generation = fixture.publish(
        2,
        &[(
            source("no-evidence"),
            vec![(1, "authored ordinary message".to_owned())],
        )],
        None,
    );
    let stale = crate::readiness(&fixture.data_root).unwrap();
    assert_eq!(stale.currentness, CoreProjectionCurrentness::Stale);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap();
    let abstained = crate::readiness(&fixture.data_root).unwrap();
    assert_eq!(abstained.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(
        abstained.materialized_coverage,
        MaterializedCoverage::Abstained
    );
    assert!(abstained.diagnostic.is_none());
    assert!(matches!(
        crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap(),
        CoreMaterializationSyncOutcome::Finished {
            did_work: false,
            ..
        }
    ));
    assert_eq!(
        fs::read(legacy.join("sentinel")).unwrap(),
        b"untouched legacy bytes"
    );
}

#[test]
fn incompatible_index_rebuilds_from_the_same_current_core_pin() {
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap();
    let store = ctx_attribution_index::SegmentStore::new(&fixture.graph_root);
    let mut manifest = store.load_active().unwrap().unwrap();
    manifest.schema_identity = format!("sha256:{}", "f".repeat(64));
    store.install_manifest_for_test(&manifest).unwrap();
    let incompatible = crate::readiness(&fixture.data_root).unwrap();
    assert_eq!(
        incompatible.currentness,
        CoreProjectionCurrentness::NeedsRebuild
    );
    assert_eq!(
        incompatible.diagnostic.unwrap().next_action.unwrap().argv,
        ["ctx", "import", "--all"]
    );
    assert!(matches!(
        crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap(),
        CoreMaterializationSyncOutcome::Finished { did_work: true, .. }
    ));
    assert_eq!(
        crate::readiness(&fixture.data_root)
            .unwrap()
            .materialized_coverage,
        MaterializedCoverage::Empty
    );
    // Corruption is an observation failure, never synthetic readiness.
    fs::write(store.active_manifest_path(), b"corrupt").unwrap();
    assert!(crate::readiness(&fixture.data_root).is_err());
}

#[test]
fn authored_positive_core_and_git_seed_uses_the_real_query_and_exact_citation() {
    let fixture = Fixture::new();
    let seed = crate::test_fixtures::authored_commit_fixture(&fixture.data_root.join("repository"))
        .unwrap();
    let mut writer = GenerationWriter::open(&fixture.index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(seed.record.source.clone()).unwrap();
    writer.add_core_record(seed.record.clone()).unwrap();
    writer.certify_source(seed.certificate).unwrap();
    let generation = writer.commit(|_| true).unwrap().generation_id;
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap();
    let target = crate::protocol::BlameTarget::Commit {
        oid: seed.oid,
        repository: None,
    };
    let hosted = crate::query(&fixture.data_root, &target, 10, None).unwrap();
    assert_eq!(
        hosted.freshness,
        crate::protocol::BlameResultFreshness::Current
    );
    assert_eq!(
        hosted.result.outcome.attribution,
        crate::protocol::BlameAttribution::Possible
    );
    assert!(!hosted.result.matches.is_empty());
    let expected = crate::protocol::EvidenceCitation {
        core_generation_id: generation,
        source: seed.record.source.clone(),
        session_id: seed.record.session_id,
        event_id: seed.record.event_id,
        event_sequence: 1,
        byte_range: None,
        evidence_sha256: Some(crate::protocol::core_record_sha256(&seed.record).unwrap()),
    };
    assert!(
        hosted
            .result
            .evidence
            .iter()
            .any(|evidence| evidence.citation == expected)
    );
}

#[test]
fn query_rejects_obsolete_materializer_even_when_core_and_format_match() {
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap();
    let store = ctx_attribution_index::SegmentStore::new(&fixture.graph_root);
    let mut manifest = store.load_active().unwrap().unwrap();
    manifest.materializer_identity = "obsolete-materializer".to_owned();
    manifest.core_receipt.materializer_revision = manifest.materializer_identity.clone();
    store.install_manifest_for_test(&manifest).unwrap();
    let target = crate::protocol::BlameTarget::Commit {
        oid: "a".repeat(40),
        repository: None,
    };
    let error = crate::query(&fixture.data_root, &target, 10, None).unwrap_err();
    assert_eq!(
        error.reason,
        crate::protocol::BlameDiagnosticReason::ProjectionIncompatible
    );
    assert_eq!(error.next_action.unwrap().argv, ["ctx", "import", "--all"]);
}

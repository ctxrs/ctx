//! Authored Core/Git inputs; no provider files or user stores are read.
use super::*;

#[test]
fn corrupt_manifest_rebuilds_and_queries_the_same_retained_core() {
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
    let baseline = crate::query(&fixture.data_root, &target, 10, None).unwrap();
    assert!(!baseline.result.matches.is_empty());
    let core_before = fs::read_dir(fixture.index_root.join("index-generations"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .flat_map(|path| fs::read_dir(path).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect::<Vec<_>>();
    assert!(!core_before.is_empty());
    let legacy = fixture.data_root.join("pro");
    fs::create_dir(&legacy).unwrap();
    let sentinels = [
        fixture.graph_root.join("user-notes.txt"),
        legacy.join("sentinel"),
    ];
    for path in &sentinels {
        fs::write(path, b"unrelated user bytes").unwrap();
    }
    let manifest =
        ctx_attribution_index::SegmentStore::new(&fixture.graph_root).active_manifest_path();
    for damage in 0..3 {
        let mut bytes = fs::read(&manifest).unwrap();
        match damage {
            0 => bytes[..4].copy_from_slice(b"BAD!"),
            1 => *bytes.last_mut().unwrap() ^= 1,
            _ => bytes.truncate(4),
        }
        fs::write(&manifest, &bytes).unwrap();
        assert_eq!(
            crate::readiness(&fixture.data_root).unwrap_err().error_code,
            "corrupt_graph"
        );
        assert!(crate::query(&fixture.data_root, &target, 10, None).is_err());
        // Observations never repair the manifest.
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
        assert!(matches!(
            crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap(),
            CoreMaterializationSyncOutcome::Finished { did_work: true, .. }
        ));
        let repaired = crate::query(&fixture.data_root, &target, 10, None).unwrap();
        assert_eq!(
            repaired.freshness,
            crate::protocol::BlameResultFreshness::Current
        );
        assert_eq!(
            repaired.result.outcome.attribution,
            crate::protocol::BlameAttribution::Possible
        );
        assert_eq!(repaired.result.evidence, baseline.result.evidence);
        assert!(matches!(
            crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap(),
            CoreMaterializationSyncOutcome::Finished {
                did_work: false,
                ..
            }
        ));
        for (path, bytes) in &core_before {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        for path in &sentinels {
            assert_eq!(fs::read(path).unwrap(), b"unrelated user bytes");
        }
        assert_eq!(
            ctx_history_snapshot_reader::load_active_generation_id(&fixture.data_root)
                .unwrap()
                .as_deref(),
            Some(generation.as_str())
        );
    }
}

#[test]
fn interrupted_rebuild_preserves_core_and_retry_completes() {
    let fixture = Fixture::new();
    let generation = fixture.publish(
        1,
        &[(source("recovery"), vec![(1, "retained".to_owned())])],
        None,
    );
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap();
    let manifest =
        ctx_attribution_index::SegmentStore::new(&fixture.graph_root).active_manifest_path();
    fs::write(&manifest, b"corrupt manifest").unwrap();
    assert!(crate::catch_up(&fixture.data_root, &snapshot, &|| true).is_err());
    assert_eq!(fs::read(&manifest).unwrap(), b"corrupt manifest");
    let error =
        crate::catch_up_with_progress(&fixture.data_root, &snapshot, &|| false, &|progress| {
            if progress.phase == crate::materializer::MaterializationPhase::Preparing {
                anyhow::bail!("authored output failure after rebuild admission");
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "authored output failure after rebuild admission"
    );
    assert_eq!(
        crate::readiness(&fixture.data_root).unwrap().currentness,
        CoreProjectionCurrentness::NotMaterialized
    );
    let reopened =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    assert_eq!(reopened.indexed_documents(), 1);
    crate::catch_up(&fixture.data_root, &reopened, &|| false).unwrap();
    assert_eq!(
        crate::readiness(&fixture.data_root).unwrap().currentness,
        CoreProjectionCurrentness::Current
    );
}

#[cfg(unix)]
#[test]
fn recovery_does_not_unlink_unsafe_manifest_paths() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    crate::catch_up(&fixture.data_root, &snapshot, &|| false).unwrap();
    let manifest =
        ctx_attribution_index::SegmentStore::new(&fixture.graph_root).active_manifest_path();
    let original = fs::read(&manifest).unwrap();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(crate::catch_up(&fixture.data_root, &snapshot, &|| false).is_err());
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read(&manifest).unwrap(), original);
    let unrelated = fixture.data_root.join("unrelated-source");
    fs::write(&unrelated, b"BAD! user source").unwrap();
    fs::remove_file(&manifest).unwrap();
    symlink(&unrelated, &manifest).unwrap();
    assert!(crate::catch_up(&fixture.data_root, &snapshot, &|| false).is_err());
    assert!(
        fs::symlink_metadata(&manifest)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&unrelated).unwrap(), b"BAD! user source");
}

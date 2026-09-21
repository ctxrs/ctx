use super::*;
use crate::{FILE_TOUCHED, FactFamily, FlatOpenPolicy, FlatSegmentWriter, FlatStore, SegmentRef};

fn private_root() -> io::Result<tempfile::TempDir> {
    let root = tempfile::tempdir()?;
    // TempDir uses ambient directory permissions, not the index root contract.
    ctx_history_platform::platform_security::establish_private_data_root(root.path())?;
    Ok(root)
}

#[cfg(unix)]
#[test]
fn staging_rejects_nonprivate_existing_root_before_writing()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir()?;
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755))?;
    let store = SegmentStore::new(temp.path());
    assert!(matches!(
        store.stage_manifest(&manifest("44", None)),
        Err(SegmentStoreError::Io { source, .. })
            if source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(
        fs::metadata(temp.path())?.permissions().mode() & 0o777,
        0o755
    );
    assert!(fs::read_dir(temp.path())?.next().is_none());
    Ok(())
}

fn manifest(generation: &str, previous: Option<&SegmentManifest>) -> SegmentManifest {
    SegmentManifest {
        schema_version: crate::MANIFEST_SCHEMA_VERSION,
        generation_id: generation.repeat(32),
        prior_generation_id: previous.map(|manifest| manifest.generation_id.clone()),
        graph_generation: previous.map_or(1, |manifest| manifest.graph_generation + 1),
        core_receipt: ctx_attribution_model::CoreMaterializationReceipt {
            core_generation_id: "11".repeat(32),
            core_record_contract_fingerprint: "22".repeat(32),
            source_snapshot_sha256: "33".repeat(32),
            materializer_revision: "test-materializer-v1".into(),
            source_count: 1,
            event_count: 2,
        },
        materializer_identity: "test-materializer-v1".into(),
        schema_identity: "schema-v1".into(),
        evidence_identity: "evidence-v1".into(),
        ordering_identity: "ordering-v1".into(),
        segments: vec![],
        predecessor_segments: previous.map_or_else(Vec::new, |manifest| manifest.segments.clone()),
    }
}

fn add_flat(
    root: &Path,
    manifest: &mut SegmentManifest,
    id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = crate::segment_file_name(&manifest.generation_id, crate::FLAT_SERVING_ROLE);
    let path = root.join(&name);
    let record =
        crate::flat::tests::record(id, "event", 1, "repo", FILE_TOUCHED, &["src/main.rs"])?;
    let writer = SegmentWriter::create(
        &path,
        manifest.generation_bytes()?,
        crate::FLAT_SERVING_ROLE,
        crate::SEGMENT_CHUNK_BYTES,
    )?;
    let stats = FlatSegmentWriter::write(writer, vec![record], vec![])?;
    use sha2::{Digest as _, Sha256};
    manifest.segments.push(SegmentRef {
        ordinal: 0,
        publication_generation: manifest.graph_generation,
        generation_id: manifest.generation_id.clone(),
        role: crate::FLAT_SERVING_ROLE,
        file_name: name,
        plaintext_bytes: stats.plaintext_bytes,
        file_sha256: hex::encode(Sha256::digest(fs::read(path)?)),
    });
    Ok(())
}

#[test]
fn publication_cas_and_interrupted_candidates_preserve_committed_frontier()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = private_root()?;
    let store = SegmentStore::new(temp.path());
    assert!(store.load_active()?.is_none());
    let first = manifest("44", None);
    let losing = store.stage_manifest(&manifest("55", None))?;
    let staged = store.stage_manifest(&first)?;
    assert!(store.load_active()?.is_none());
    store.publish_candidate(staged)?;
    assert!(matches!(
        store.publish_candidate(losing),
        Err(SegmentStoreError::CompareAndSwap { .. })
    ));
    let second = manifest("66", Some(&first));
    let interrupted = store.stage_manifest(&second)?;
    let abandoned = interrupted.path().to_owned();
    drop(interrupted);
    assert!(!abandoned.exists());
    assert_eq!(store.load_active()?, Some(first.clone()));
    store.publish_candidate(store.stage_manifest(&second)?)?;
    assert_eq!(store.load_active()?, Some(second));
    Ok(())
}

#[test]
fn corrupt_candidate_cannot_replace_valid_generation() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Seek as _, SeekFrom};
    let temp = private_root()?;
    let store = SegmentStore::new(temp.path());
    let first = manifest("44", None);
    store.publish_candidate(store.stage_manifest(&first)?)?;
    let candidate = store.stage_manifest(&manifest("55", Some(&first)))?;
    let mut file = fs::OpenOptions::new().write(true).open(candidate.path())?;
    file.seek(SeekFrom::Start(crate::SEGMENT_HEADER_BYTES + 3))?;
    file.write_all(b"x")?;
    drop(file);
    assert!(store.publish_candidate(candidate).is_err());
    assert_eq!(store.load_active()?, Some(first));
    Ok(())
}

#[test]
fn generation_pin_and_receipt_stay_together_after_new_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = private_root()?;
    let store = SegmentStore::new(temp.path());
    let mut first = manifest("44", None);
    add_flat(temp.path(), &mut first, "old-record")?;
    store.publish_candidate(store.stage_manifest(&first)?)?;
    let policy = FlatOpenPolicy::new("schema-v1", "evidence-v1", "ordering-v1");
    let pin = FlatStore::new(temp.path()).open_active(policy)?;
    let mut second = manifest("55", Some(&first));
    second.core_receipt.core_generation_id = "66".repeat(32);
    add_flat(temp.path(), &mut second, "new-record")?;
    store.publish_candidate(store.stage_manifest(&second)?)?;
    let next = FlatStore::new(temp.path()).open_active(policy)?;
    assert_eq!(pin.generation_id(), first.generation_id);
    assert_eq!(pin.completed_receipt(), &first.core_receipt);
    assert_eq!(next.generation_id(), second.generation_id);
    #[cfg(unix)]
    fs::remove_file(temp.path().join(&first.segments[0].file_name))?;
    let family = FactFamily::new(FILE_TOUCHED)?;
    let read = |pin: &crate::PinnedFlatGeneration| -> Result<String, Box<dyn std::error::Error>> {
        let page = pin.query_exact_page(
            pin.first_reader()?.ok_or("missing reader")?,
            "repo",
            &family,
            "src/main.rs",
            None,
        )?;
        Ok(page
            .records
            .first()
            .ok_or("missing record")?
            .record_id
            .clone())
    };
    assert_eq!(read(&pin)?, "old-record");
    assert_eq!(read(&next)?, "new-record");
    Ok(())
}

#[test]
fn sibling_legacy_bytes_are_untouched_and_never_selected() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let legacy = temp.path().join("pro");
    fs::create_dir(&legacy)?;
    for name in [
        "graph-manifest.ctxm",
        "graph-segment-legacy.ctxs",
        "vault.bin",
    ] {
        fs::write(legacy.join(name), b"authored inert legacy bytes")?;
    }
    let root = temp.path().join("search/attribution");
    fs::create_dir_all(&root)?;
    ctx_history_platform::platform_security::restrict_private_directory(&root)?;
    let misplaced_legacy = root.join("graph-manifest.ctxm");
    fs::write(&misplaced_legacy, b"authored ignored legacy manifest")?;
    let store = SegmentStore::new(&root);
    assert!(store.load_active()?.is_none());
    let first = manifest("44", None);
    store.publish_candidate(store.stage_manifest(&first)?)?;
    store.cleanup_candidates()?;
    assert_eq!(
        fs::read(&misplaced_legacy)?,
        b"authored ignored legacy manifest"
    );
    for name in [
        "graph-manifest.ctxm",
        "graph-segment-legacy.ctxs",
        "vault.bin",
    ] {
        assert_eq!(fs::read(legacy.join(name))?, b"authored inert legacy bytes");
    }
    Ok(())
}

#[test]
fn native_lock_rejects_competing_publisher() -> Result<(), Box<dyn std::error::Error>> {
    let temp = private_root()?;
    let store = SegmentStore::new(temp.path());
    let candidate = store.stage_manifest(&manifest("44", None))?;
    let held = PublicationLock::acquire(temp.path())?;
    assert!(matches!(
        store.publish_candidate(candidate),
        Err(SegmentStoreError::PublicationBusy)
    ));
    drop(held);
    let candidate = store.stage_manifest(&manifest("44", None))?;
    assert!(store.publish_candidate(candidate).is_ok());
    Ok(())
}

#[test]
fn post_activation_sync_failure_reports_visible_commit_without_rollback()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = private_root()?;
    let store = SegmentStore::new(temp.path());
    let first = manifest("44", None);
    store.publish_candidate(store.stage_manifest(&first)?)?;
    let second = manifest("55", Some(&first));
    store.fail_next_post_activation_sync_for_test()?;
    assert!(matches!(
        store.publish_candidate(store.stage_manifest(&second)?),
        Err(SegmentStoreError::ActivationDurabilityUncertain { .. })
    ));
    assert_eq!(store.load_active()?, Some(second.clone()));
    let third = manifest("66", Some(&second));
    assert!(
        store
            .publish_candidate(store.stage_manifest(&third)?)
            .is_ok()
    );
    Ok(())
}

#[test]
fn opening_retries_when_two_publications_retire_selected_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = private_root()?;
    let root = temp.path();
    let store = SegmentStore::new(root);
    let mut first = manifest("44", None);
    add_flat(root, &mut first, "old-record")?;
    store.publish_candidate(store.stage_manifest(&first)?)?;
    let mut second = manifest("55", Some(&first));
    add_flat(root, &mut second, "middle-record")?;
    let mut third = manifest("66", Some(&second));
    add_flat(root, &mut third, "new-record")?;
    let policy = FlatOpenPolicy::new("schema-v1", "evidence-v1", "ordering-v1");
    let mut publication = Ok(());
    let pin = FlatStore::new(root).open_active_after_selection_for_test(policy, || {
        publication = (|| -> Result<(), Box<dyn std::error::Error>> {
            store.publish_candidate(store.stage_manifest(&second)?)?;
            store.publish_candidate(store.stage_manifest(&third)?)?;
            fs::remove_file(root.join(&first.segments[0].file_name))?;
            Ok(())
        })();
    });
    publication?;
    let pin = pin?;
    assert_eq!(pin.generation_id(), third.generation_id);
    let page = pin.query_exact_page(
        pin.first_reader()?.ok_or("missing reader")?,
        "repo",
        &FactFamily::new(FILE_TOUCHED)?,
        "src/main.rs",
        None,
    )?;
    assert_eq!(page.records[0].record_id, "new-record");
    Ok(())
}

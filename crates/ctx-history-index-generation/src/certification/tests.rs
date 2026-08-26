use super::*;

#[cfg(unix)]
struct ReadOnlyCertificationFixture {
    temp: tempfile::TempDir,
    slot: GenerationSlot,
    index: tantivy::Index,
    relative_artifact_path: PathBuf,
    certified_artifact: ArtifactIdentity,
}

#[cfg(unix)]
impl ReadOnlyCertificationFixture {
    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn artifact_path(&self) -> PathBuf {
        slot_path(self.root(), &self.slot).join(&self.relative_artifact_path)
    }
}

#[cfg(unix)]
fn read_only_certification_fixture() -> ReadOnlyCertificationFixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut schema = tantivy::schema::Schema::builder();
    let body = schema.add_text_field("body", tantivy::schema::TEXT | tantivy::schema::STORED);
    let candidate =
        crate::create_candidate_generation(root, None, schema.build(), 50_000_000).unwrap();
    let directory_name = candidate.directory_name.clone();
    let index = candidate.index;
    let mut writer = index.writer(50_000_000).unwrap();
    writer
        .add_document(tantivy::doc!(body => "immutable payload"))
        .unwrap();
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();

    let generation_path = root.join(INDEX_GENERATIONS_DIRECTORY).join(&directory_name);
    let audit = physical_integrity_audit(&index, &generation_path, None).unwrap();
    let slot =
        GenerationSlot::new("1".repeat(64), directory_name, audit.digest().to_owned()).unwrap();
    let pointer = ActiveGenerationPointer::new(slot.clone(), None).unwrap();
    fs::create_dir_all(root.join(MANIFEST_DIRECTORY)).unwrap();
    fs::write(manifest_path(root, slot.generation_id()), b"manifest").unwrap();
    crate::publish_active_generation_pointer(root, &pointer).unwrap();
    let certified = install_certification(
        root,
        Some(&pointer),
        None,
        &slot,
        &index,
        &audit,
        CertificationInstallPolicy::ACTIVE_CACHE,
    )
    .unwrap();
    let relative_artifact_path = active_index_files(&index)
        .unwrap()
        .into_iter()
        .find(|path| {
            fs::metadata(generation_path.join(path)).is_ok_and(|metadata| metadata.len() > 0)
        })
        .unwrap();
    let (certified_artifact, _, sealed) = certified
        .certified_artifact(&relative_artifact_path)
        .unwrap();
    assert!(sealed);
    assert!(certified_artifact.identity.is_readonly());

    ReadOnlyCertificationFixture {
        temp,
        slot,
        index,
        relative_artifact_path,
        certified_artifact,
    }
}

#[cfg(unix)]
#[test]
fn candidate_certification_rejects_a_slot_with_a_different_physical_digest() {
    let fixture = read_only_certification_fixture();
    let pointer = load_current_pointer(fixture.root()).unwrap();
    let generation_path = slot_path(fixture.root(), &fixture.slot);
    let audit = physical_integrity_audit(&fixture.index, &generation_path, Some(&pointer)).unwrap();
    let mismatched_slot = GenerationSlot::new(
        fixture.slot.generation_id().to_owned(),
        fixture.slot.directory().to_owned(),
        "0".repeat(64),
    )
    .unwrap();
    let predecessor_fence =
        ActiveGenerationPointerFence::capture(fixture.root(), Some(&pointer)).unwrap();

    assert!(matches!(
        certify_candidate_physical_integrity(
            fixture.root(),
            &predecessor_fence,
            &mismatched_slot,
            &fixture.index,
            &audit,
        ),
        Err(IndexError::ChecksumMismatch)
    ));
}

#[cfg(unix)]
fn mutate_same_length_and_restore_metadata(path: &Path) -> (Metadata, Metadata) {
    use std::{io::Write as _, os::unix::fs::PermissionsExt as _};

    let before = fs::metadata(path).unwrap();
    let original_permissions = before.permissions();
    let modified = before.modified().unwrap();
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 0x5a;

    let mut writable = original_permissions.clone();
    writable.set_mode(writable.mode() | 0o200);
    fs::set_permissions(path, writable).unwrap();
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.write_all(&bytes).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs::set_permissions(path, original_permissions).unwrap();

    (before, fs::metadata(path).unwrap())
}

fn generation(root: &Path, digit: char) -> PathBuf {
    root.join(INDEX_GENERATIONS_DIRECTORY)
        .join(format!("generation-{}", digit.to_string().repeat(32)))
}

fn pointer(digit: char) -> ActiveGenerationPointer {
    let digit = digit.to_string();
    ActiveGenerationPointer::new(
        GenerationSlot::new(
            digit.repeat(64),
            format!("generation-{}", digit.repeat(32)),
            digit.repeat(64),
        )
        .unwrap(),
        None,
    )
    .unwrap()
}

#[test]
fn managed_link_creation_and_cleanup_are_retryable_stable_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let active = generation(root, '1');
    let candidate = generation(root, '2');
    fs::create_dir_all(&active).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    let relative = Path::new("payload.bin");
    let active_path = active.join(relative);
    let candidate_path = candidate.join(relative);
    fs::write(&active_path, b"immutable payload").unwrap();

    let (file, before_link) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
    fs::hard_link(&active_path, &candidate_path).unwrap();
    assert!(matches!(
        stable_artifact_link_snapshot(root, &active_path, relative, &file, &before_link, None,)
            .unwrap(),
        ArtifactLinkSnapshot::Retry
    ));
    drop(file);

    let (_, linked) = open_artifact(root, &active, relative, None).unwrap();
    assert_eq!(linked.identity.link_count(), 2);
    let (file, before_unlink) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
    fs::remove_file(&candidate_path).unwrap();
    assert!(matches!(
        stable_artifact_link_snapshot(root, &active_path, relative, &file, &before_unlink, None,)
            .unwrap(),
        ArtifactLinkSnapshot::Retry
    ));
    drop(file);

    let (_, unlinked) = open_artifact(root, &active, relative, None).unwrap();
    assert_eq!(unlinked.identity.link_count(), 1);
    assert!(linked.same_payload_identity_changed(&unlinked));
}

#[cfg(unix)]
#[test]
fn activation_certification_rejects_mutation_masked_by_link_reclamation() {
    let fixture = read_only_certification_fixture();
    let root = fixture.root();
    let generation_path = slot_path(root, &fixture.slot);
    let artifact_path = fixture.artifact_path();
    let candidate_generation = generation(root, 'f');
    let candidate_artifact = candidate_generation.join(&fixture.relative_artifact_path);
    fs::create_dir_all(candidate_artifact.parent().unwrap()).unwrap();
    fs::hard_link(&artifact_path, &candidate_artifact).unwrap();
    let pointer = load_current_pointer(root).unwrap();
    let audit = physical_integrity_audit(&fixture.index, &generation_path, Some(&pointer)).unwrap();

    let certified_bytes = fs::read(&artifact_path).unwrap();
    mutate_same_length_and_restore_metadata(&artifact_path);
    assert_ne!(fs::read(&artifact_path).unwrap(), certified_bytes);
    fs::remove_file(&candidate_artifact).unwrap();
    fs::remove_dir(&candidate_generation).unwrap();

    crate::reset_physical_verification_activity();
    assert!(matches!(
        certify_activated_generation(root, &pointer, &fixture.slot, &fixture.index, &audit,),
        Err(IndexError::ConcurrentGenerationChange)
    ));
    assert_eq!(crate::checksum_walks(), 0);
    assert_eq!(crate::hashed_artifact_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn read_only_certification_rejects_restored_metadata_byte_mutation() {
    use std::os::unix::fs::MetadataExt as _;

    let fixture = read_only_certification_fixture();
    let artifact_path = fixture.artifact_path();
    let (before, after) = mutate_same_length_and_restore_metadata(&artifact_path);
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert_eq!(after.mode(), before.mode());
    assert!(after.permissions().readonly());
    assert_eq!(after.nlink(), before.nlink());

    crate::reset_physical_verification_activity();
    assert!(matches!(
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::checksum_walks(), 0);
    assert_eq!(crate::hashed_artifact_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn read_only_certification_rejects_unretained_alias_and_restored_metadata_mutation() {
    use std::os::unix::fs::MetadataExt as _;

    let fixture = read_only_certification_fixture();
    let artifact_path = fixture.artifact_path();
    let attacker_generation = generation(fixture.root(), 'd');
    let external_alias = attacker_generation.join(&fixture.relative_artifact_path);
    fs::create_dir_all(external_alias.parent().unwrap()).unwrap();
    fs::hard_link(&artifact_path, &external_alias).unwrap();
    assert_eq!(
        fs::metadata(&artifact_path).unwrap().nlink(),
        fixture.certified_artifact.identity.link_count() + 1
    );

    let (before, after) = mutate_same_length_and_restore_metadata(&artifact_path);
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert_eq!(after.mode(), before.mode());
    assert!(after.permissions().readonly());
    assert_eq!(after.nlink(), before.nlink());

    crate::reset_physical_verification_activity();
    assert!(matches!(
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::checksum_walks(), 0);
    assert_eq!(crate::hashed_artifact_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn read_only_certification_rejects_accounted_link_transition_without_hashing() {
    use std::os::unix::fs::MetadataExt as _;

    let fixture = read_only_certification_fixture();
    let artifact_path = fixture.artifact_path();
    let linked_generation = generation(fixture.root(), 'e');
    let linked_artifact = linked_generation.join(&fixture.relative_artifact_path);
    fs::create_dir_all(linked_artifact.parent().unwrap()).unwrap();
    fs::hard_link(&artifact_path, &linked_artifact).unwrap();
    let linked_slot = GenerationSlot::new(
        "e".repeat(64),
        format!("generation-{}", "e".repeat(32)),
        "e".repeat(64),
    )
    .unwrap();
    let pointer = ActiveGenerationPointer::new(linked_slot, Some(fixture.slot.clone())).unwrap();
    crate::publish_active_generation_pointer(fixture.root(), &pointer).unwrap();

    let linked = fs::metadata(&artifact_path).unwrap();
    assert_eq!(
        linked.nlink(),
        fixture.certified_artifact.identity.link_count() + 1
    );
    crate::reset_physical_verification_activity();
    assert!(matches!(
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::checksum_walks(), 0);
    assert_eq!(crate::hashed_artifact_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn read_only_certification_rejects_mutation_masked_by_accounted_link_transition() {
    use std::os::unix::fs::MetadataExt as _;

    let fixture = read_only_certification_fixture();
    let artifact_path = fixture.artifact_path();
    let certified_bytes = fs::read(&artifact_path).unwrap();
    let (before_mutation, after_mutation) = mutate_same_length_and_restore_metadata(&artifact_path);
    assert_ne!(fs::read(&artifact_path).unwrap(), certified_bytes);
    assert_eq!(after_mutation.len(), before_mutation.len());
    assert_eq!(
        after_mutation.modified().unwrap(),
        before_mutation.modified().unwrap()
    );
    assert_eq!(after_mutation.mode(), before_mutation.mode());
    assert_eq!(after_mutation.nlink(), before_mutation.nlink());

    let linked_generation = generation(fixture.root(), 'e');
    let linked_artifact = linked_generation.join(&fixture.relative_artifact_path);
    fs::create_dir_all(linked_artifact.parent().unwrap()).unwrap();
    fs::hard_link(&artifact_path, &linked_artifact).unwrap();
    let linked_slot = GenerationSlot::new(
        "e".repeat(64),
        format!("generation-{}", "e".repeat(32)),
        "e".repeat(64),
    )
    .unwrap();
    let pointer = ActiveGenerationPointer::new(linked_slot, Some(fixture.slot.clone())).unwrap();
    crate::publish_active_generation_pointer(fixture.root(), &pointer).unwrap();

    let linked = fs::metadata(&artifact_path).unwrap();
    assert_eq!(
        linked.nlink(),
        fixture.certified_artifact.identity.link_count() + 1
    );
    crate::reset_physical_verification_activity();
    assert!(matches!(
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::checksum_walks(), 0);
    assert_eq!(crate::hashed_artifact_bytes(), 0);
}

#[test]
fn generation_disappearing_during_alias_scan_is_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let active = generation(root, '1');
    let candidate = generation(root, '2');
    fs::create_dir_all(&active).unwrap();
    fs::create_dir_all(&candidate).unwrap();
    let relative = Path::new("payload.bin");
    let active_path = active.join(relative);
    let candidate_path = candidate.join(relative);
    fs::write(&active_path, b"immutable payload").unwrap();
    fs::hard_link(&active_path, &candidate_path).unwrap();
    let (file, linked) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();

    let candidate_for_hook = candidate.clone();
    let candidate_path_for_hook = candidate_path.clone();
    let _hook = AliasEntryTestHookGuard::install(move |entry_path| {
        if entry_path == candidate_for_hook {
            fs::remove_file(&candidate_path_for_hook).unwrap();
            fs::remove_dir(&candidate_for_hook).unwrap();
        }
    });

    assert!(matches!(
        stable_artifact_link_snapshot(root, &active_path, relative, &file, &linked, None,).unwrap(),
        ArtifactLinkSnapshot::Retry
    ));
}

#[test]
fn stale_directory_entry_errors_are_retryable_but_io_errors_are_not() {
    assert!(retryable_alias_snapshot_error(&std::io::Error::from(
        std::io::ErrorKind::NotFound,
    )));
    assert!(!retryable_alias_snapshot_error(&std::io::Error::from(
        std::io::ErrorKind::PermissionDenied,
    )));
    #[cfg(unix)]
    assert!(retryable_alias_snapshot_error(
        &std::io::Error::from_raw_os_error(libc::ESTALE)
    ));
}

#[test]
fn pointer_replacement_during_control_capture_is_concurrent_not_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let first = pointer('1');
    let second = pointer('2');
    let target = root.join("active-generation.json");
    fs::write(&target, serde_json::to_vec(&first).unwrap()).unwrap();
    let directory = DurableMmapDirectory::open(root).unwrap();
    let target_for_hook = target.clone();
    let second_bytes = serde_json::to_vec(&second).unwrap();
    let mut replaced = false;
    let hook = RegularFileIdentityTestHookGuard::install(move |path| {
        if path == target_for_hook && !replaced {
            directory
                .atomic_write(Path::new("active-generation.json"), &second_bytes)
                .unwrap();
            replaced = true;
        }
    });

    assert!(matches!(
        capture_pointer_bound_single_link_control(root, &first, &target),
        Err(IndexError::ConcurrentGenerationChange)
    ));
    drop(hook);
    assert_eq!(load_current_pointer(root).unwrap(), second);

    let directory = DurableMmapDirectory::open(root).unwrap();
    let target_for_hook = target.clone();
    let second_bytes = serde_json::to_vec(&second).unwrap();
    let mut rewritten = false;
    let hook = RegularFileIdentityTestHookGuard::install(move |path| {
        if path == target_for_hook && !rewritten {
            directory
                .atomic_write(Path::new("active-generation.json"), &second_bytes)
                .unwrap();
            rewritten = true;
        }
    });
    assert!(capture_pointer_bound_single_link_control(root, &second, &target).is_ok());
    drop(hook);

    fs::hard_link(&target, root.join("unmanaged-pointer-hardlink")).unwrap();
    assert!(matches!(
        capture_pointer_bound_single_link_control(root, &second, &target),
        Err(IndexError::ChecksumMismatch)
    ));
}

#[test]
fn stable_unmanaged_hardlink_remains_checksum_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let active = generation(root, '1');
    fs::create_dir_all(&active).unwrap();
    let relative = Path::new("payload.bin");
    let active_path = active.join(relative);
    fs::write(&active_path, b"immutable payload").unwrap();
    fs::hard_link(&active_path, root.join("unmanaged-hardlink")).unwrap();

    assert!(matches!(
        open_artifact(root, &active, relative, None),
        Err(IndexError::ChecksumMismatch)
    ));
}

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
#[test]
fn active_storage_metadata_sums_only_certified_artifacts_without_hashing() {
    let fixture = read_only_certification_fixture();
    let certification = read_certification(&certification_path(fixture.root(), &fixture.slot))
        .and_then(|bytes| serde_json::from_slice::<GenerationIntegrityCertification>(&bytes).ok())
        .unwrap();
    let expected = certification
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact.identity.length())
        .sum::<u64>();
    let alias_entries = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let observed_alias_entries = std::rc::Rc::clone(&alias_entries);
    let _alias_hook = AliasEntryTestHookGuard::install(move |_| {
        observed_alias_entries.set(observed_alias_entries.get().saturating_add(1));
    });

    crate::reset_physical_verification_activity();
    let actual = active_generation_storage_metadata(fixture.root())
        .unwrap()
        .unwrap();

    assert_eq!(actual.generation_id(), fixture.slot.generation_id());
    assert_eq!(actual.logical_bytes(), expected);
    assert_eq!(crate::checksum_walks(), 0);
    assert_eq!(crate::hashed_artifact_bytes(), 0);
    assert_eq!(alias_entries.get(), 0);
}

#[test]
fn active_storage_metadata_is_absent_without_a_generation() {
    let root = tempfile::tempdir().unwrap();
    assert!(active_generation_storage_metadata(root.path())
        .unwrap()
        .is_none());
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
    assert_eq!(crate::checksum_walks(), 1);
    assert!(crate::hashed_artifact_bytes() > 0);
}

#[cfg(unix)]
#[test]
fn read_only_certification_rejects_unretained_alias_in_rebuilt_certification() {
    use std::os::unix::fs::MetadataExt as _;

    let fixture = read_only_certification_fixture();
    let root = fixture.root();
    let artifact_path = fixture.artifact_path();
    fs::remove_file(certification_path(root, &fixture.slot)).unwrap();
    let attacker_generation = generation(root, 'd');
    let external_alias = attacker_generation.join(&fixture.relative_artifact_path);
    fs::create_dir_all(external_alias.parent().unwrap()).unwrap();
    fs::hard_link(&artifact_path, &external_alias).unwrap();
    let pointer = load_current_pointer(root).unwrap();
    let generation_path = slot_path(root, &fixture.slot);
    let audit = physical_integrity_audit(&fixture.index, &generation_path, Some(&pointer)).unwrap();
    install_certification(
        root,
        Some(&pointer),
        None,
        &fixture.slot,
        &fixture.index,
        &audit,
        CertificationInstallPolicy::ACTIVE_CACHE,
    )
    .unwrap();
    let certification: GenerationIntegrityCertification = serde_json::from_slice(
        &read_certification(&certification_path(root, &fixture.slot)).unwrap(),
    )
    .unwrap();
    let certified_artifact = certification
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact.path == fixture.relative_artifact_path.to_str().unwrap())
        .unwrap();
    assert_eq!(
        certified_artifact.artifact.identity.link_count(),
        fixture.certified_artifact.identity.link_count() + 1
    );
    assert_eq!(
        fs::metadata(&artifact_path).unwrap().nlink(),
        certified_artifact.artifact.identity.link_count()
    );

    crate::reset_physical_verification_activity();
    assert!(matches!(
        verify_physical_integrity_read_only(root, &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::checksum_walks(), 0);
    assert_eq!(crate::hashed_artifact_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn read_only_active_snapshot_rehashes_during_candidate_hardlinking() {
    let fixture = read_only_certification_fixture();
    let candidate = generation(fixture.root(), 'e');
    let candidate_artifact = candidate.join(&fixture.relative_artifact_path);
    fs::create_dir_all(candidate_artifact.parent().unwrap()).unwrap();
    fs::hard_link(fixture.artifact_path(), &candidate_artifact).unwrap();

    crate::reset_physical_verification_activity();
    assert!(matches!(
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::checksum_walks(), 0);
    let _writer = crate::retention::acquire_candidate_generation_directory_read_authority(
        fixture.root(),
        candidate.file_name().unwrap().to_str().unwrap(),
    )
    .unwrap();

    crate::reset_physical_verification_activity();
    verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index).unwrap();
    assert_eq!(crate::checksum_walks(), 1);
    assert!(crate::hashed_artifact_bytes() > 0);

    mutate_same_length_and_restore_metadata(&candidate_artifact);
    assert!(matches!(
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
}

#[cfg(unix)]
#[test]
fn read_only_active_snapshot_rejects_writable_artifact_without_hashing() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = read_only_certification_fixture();
    let artifact_path = fixture.artifact_path();
    let mut permissions = fs::metadata(&artifact_path).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(artifact_path, permissions).unwrap();

    crate::reset_physical_verification_activity();
    assert!(matches!(
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::checksum_walks(), 0);
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
    // A retained older directory does not make a static external alias a
    // generation race. Missing files in an ordinary peer remain supported.
    fs::create_dir_all(generation(root, '2')).unwrap();
    let pointer = pointer('1');
    open_artifact(root, &active, relative, Some(&pointer)).unwrap();
    fs::hard_link(&active_path, root.join("unmanaged-hardlink")).unwrap();

    for topology in [None, Some(&pointer)] {
        assert!(matches!(
            open_artifact(root, &active, relative, topology),
            Err(IndexError::ChecksumMismatch)
        ));
    }
}

#[cfg(unix)]
#[test]
fn alias_open_preserves_descriptor_exhaustion() {
    const CHILD: &str = "CTX_CERTIFICATION_ALIAS_FD_EXHAUSTION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "certification::tests::alias_open_preserves_descriptor_exhaustion",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let active = generation(temp.path(), '1');
    fs::create_dir_all(&active).unwrap();
    fs::create_dir_all(generation(temp.path(), '2')).unwrap();
    fs::write(active.join("payload.bin"), b"payload").unwrap();
    let limit = libc::rlimit {
        rlim_cur: 64,
        rlim_max: 64,
    };
    // This process is an isolated test child; the parent retains its limit.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let mut descriptors = Vec::new();
    let hook = AliasEntryTestHookGuard::install(move |_| {
        while let Ok(file) = File::open("/dev/null") {
            descriptors.push(file);
        }
    });
    let error = open_artifact(
        temp.path(),
        &active,
        Path::new("payload.bin"),
        Some(&pointer('1')),
    )
    .unwrap_err();
    drop(hook);
    assert!(
        matches!(error, IndexError::Io(ref io) if io.raw_os_error() == Some(libc::EMFILE)),
        "{error:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn delayed_reader_cache_cannot_overwrite_completed_managed_link_refresh() {
    let fixture = read_only_certification_fixture();
    let root = fixture.root();
    let pointer = load_current_pointer(root).unwrap();
    let stale = scrub_and_certify_physical_integrity(root, &pointer, &fixture.slot, &fixture.index)
        .unwrap();
    let _clone_options = crate::CloneTestHookGuard::set(
        crate::CloneTestOptions {
            force_reflink_fallback: true,
            ..Default::default()
        },
        |_, _| Ok(()),
    );
    let _candidate =
        crate::create_authenticated_candidate_generation(root, &pointer, &fixture.index, 0)
            .unwrap();
    assert!(crate::candidate_clone_metrics().retained_hardlinked_files > 0);
    let path = certification_path(root, &fixture.slot);
    let refreshed = fs::read(&path).unwrap();
    assert!(matches!(
        cache_recertified_physical_integrity(root, &pointer, &fixture.slot, &fixture.index, &stale),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(fs::read(path).unwrap(), refreshed);
    crate::reset_physical_verification_activity();
    verify_or_certify_physical_integrity(root, &pointer, &fixture.slot, &fixture.index).unwrap();
    assert_eq!(crate::hashed_artifact_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn cold_read_only_snapshots_never_initialize_or_require_writable_coordinator() {
    use std::os::unix::fs::PermissionsExt as _;

    fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, (FileIdentity, Vec<u8>)> {
        let mut files = std::collections::BTreeMap::new();
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(snapshot(&path));
            } else {
                let file = open_nofollow(&path).unwrap();
                files.insert(
                    path.clone(),
                    (file_identity(&file).unwrap(), fs::read(path).unwrap()),
                );
            }
        }
        files
    }

    for (existing_coordinator, root_mode, stale) in [
        (false, 0o700, false),
        (false, 0o500, false),
        (true, 0o500, false),
        (false, 0o700, true),
        (false, 0o500, true),
        (true, 0o500, true),
    ] {
        let fixture = read_only_certification_fixture();
        let root = fixture.root();
        // Inspecting an unrelated directory for an existing reader must not
        // initialize coordination either, or grant that directory authority.
        fs::create_dir(generation(root, 'f')).unwrap();
        if stale {
            // A completed benign link/unlink leaves immutable bytes intact,
            // but ctime must force a full audit instead of reusing the SHA.
            let alias = root.join("temporary-alias");
            fs::hard_link(fixture.artifact_path(), &alias).unwrap();
            fs::remove_file(alias).unwrap();
        }
        // No live guard survives this call. This exercises the cold opener,
        // including existing coordinator files that deny O_RDWR access.
        crate::retention::ensure_generation_read_lease_coordinator(root).unwrap();
        for name in [
            ".ctx-generation-read-leases-v2.lock",
            ".ctx-generation-lease-coordinator-init-v2.lock",
        ] {
            let path = root.join(name);
            if existing_coordinator {
                fs::set_permissions(path, fs::Permissions::from_mode(0o400)).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
        }
        fs::set_permissions(root, fs::Permissions::from_mode(root_mode)).unwrap();
        let before = snapshot(root);
        crate::reset_physical_verification_activity();
        verify_physical_integrity_read_only(root, &fixture.slot, &fixture.index).unwrap();
        assert_eq!(crate::hashed_artifact_bytes() > 0, stale);
        assert_eq!(snapshot(root), before);

        if stale {
            mutate_same_length_and_restore_metadata(&fixture.artifact_path());
            let before = snapshot(root);
            assert!(matches!(
                verify_physical_integrity_read_only(root, &fixture.slot, &fixture.index),
                Err(IndexError::ChecksumMismatch)
            ));
            assert_eq!(snapshot(root), before);
        }
        fs::write(certification_path(root, &fixture.slot), b"{").unwrap();
        let before = snapshot(root);
        assert!(matches!(
            verify_physical_integrity_read_only(root, &fixture.slot, &fixture.index),
            Err(IndexError::ChecksumMismatch)
        ));
        assert_eq!(snapshot(root), before);
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn exact_readers_finish_while_guarded_full_audit_is_paused() {
    use crate::{PublicationIoProbe, PublicationIoProbeGuard};
    use std::{sync::mpsc, time::Duration};

    let fixture = read_only_certification_fixture();
    let pointer = load_current_pointer(fixture.root()).unwrap();
    std::thread::scope(|scope| {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let audit_fixture = &fixture;
        let audit_pointer = &pointer;
        let audit = scope.spawn(move || {
            // Candidate reclamation may audit under this same exclusive guard.
            let _guard =
                crate::retention::CertificationGuard::update(audit_fixture.root()).unwrap();
            let _probe = PublicationIoProbeGuard::set(move |event| {
                if event == PublicationIoProbe::PhysicalAudit {
                    entered_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                }
                Ok(())
            });
            physical_integrity_audit(
                &audit_fixture.index,
                &slot_path(audit_fixture.root(), &audit_fixture.slot),
                Some(audit_pointer),
            )
            .unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let _probe = PublicationIoProbeGuard::set(|event| {
            assert_ne!(event, PublicationIoProbe::CertificationWait);
            Ok(())
        });
        crate::reset_physical_verification_activity();
        verify_or_certify_physical_integrity(
            fixture.root(),
            &pointer,
            &fixture.slot,
            &fixture.index,
        )
        .unwrap();
        verify_physical_integrity_read_only(fixture.root(), &fixture.slot, &fixture.index).unwrap();
        assert_eq!(crate::hashed_artifact_bytes(), 0);
        resume_tx.send(()).unwrap();
        assert_eq!(
            audit.join().unwrap().digest(),
            fixture.slot.physical_integrity_digest()
        );
    });
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn candidate_clone_waits_for_a_running_full_audit_then_finishes() {
    use crate::{PublicationIoProbe, PublicationIoProbeGuard};
    use std::{sync::mpsc, time::Duration};

    let fixture = read_only_certification_fixture();
    let pointer = load_current_pointer(fixture.root()).unwrap();
    std::thread::scope(|scope| {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let audit_fixture = &fixture;
        let audit_pointer = &pointer;
        let audit = scope.spawn(move || {
            let _probe = PublicationIoProbeGuard::set(move |event| {
                if event == PublicationIoProbe::PhysicalAudit {
                    entered_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                }
                Ok(())
            });
            scrub_and_certify_physical_integrity(
                audit_fixture.root(),
                audit_pointer,
                &audit_fixture.slot,
                &audit_fixture.index,
            )
            .unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let clone_fixture = &fixture;
        let clone_pointer = &pointer;
        let candidate = scope.spawn(move || {
            let mut waiting_tx = Some(waiting_tx);
            let _probe = PublicationIoProbeGuard::set(move |event| {
                if event == PublicationIoProbe::CertificationWait {
                    if let Some(tx) = waiting_tx.take() {
                        tx.send(()).unwrap();
                    }
                }
                Ok(())
            });
            crate::create_authenticated_candidate_generation(
                clone_fixture.root(),
                clone_pointer,
                &clone_fixture.index,
                50_000_000,
            )
            .unwrap()
        });
        waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!candidate.is_finished());
        resume_tx.send(()).unwrap();
        audit.join().unwrap();
        let candidate = candidate.join().unwrap();
        verify_or_certify_physical_integrity(
            fixture.root(),
            &pointer,
            &fixture.slot,
            &fixture.index,
        )
        .unwrap();
        drop(candidate);
    });
}

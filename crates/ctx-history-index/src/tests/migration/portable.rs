use super::*;

#[test]
fn portable_copy_preserves_permissions_and_writer_availability() {
    let predecessor = GoldenPredecessor::copy();
    let held_reader = VerifiedIndex::open(predecessor.root()).unwrap();
    let source_generation = active_generation_path(predecessor.root());
    let source_file = fs::read_dir(&source_generation)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("store"))
        .unwrap();
    let file_name = source_file.file_name().unwrap().to_owned();
    let mut permissions = fs::metadata(&source_file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&source_file, permissions).unwrap();
    let guard = PortableCloneTestGuard::set(PortableCloneTestOptions::default(), |_, _| Ok(()));

    let writer = GenerationWriter::open(predecessor.root(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let metrics: PortableCloneMetrics = guard.metrics();
    assert!(metrics.planned_files > 2);
    assert_eq!(metrics.copied_files, metrics.planned_files);
    assert_eq!(metrics.copied_bytes, metrics.logical_bytes);
    assert!(metrics.required_headroom > metrics.logical_bytes);
    assert!(metrics.available_bytes >= metrics.required_headroom);
    assert!(writer.base_manifest().is_some());
    assert!(
        fs::metadata(active_generation_path(predecessor.root()).join(file_name))
            .unwrap()
            .permissions()
            .readonly()
    );
    assert_eq!(held_reader.count_term("evidence").unwrap(), 3);
    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    assert!(!current.uses_allowlisted_predecessor_contract());
    assert_eq!(current.count_term("evidence").unwrap(), 3);
}

#[test]
fn portable_copy_failure_is_previsibility_and_retryable() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let guard = PortableCloneTestGuard::set(PortableCloneTestOptions::default(), |stage, _| {
        if stage == PortableCloneStage::BeforeCopy {
            return Err(io::Error::other("injected portable copy failure").into());
        }
        Ok(())
    });

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::Io(ref error) if error.to_string() == "injected portable copy failure"
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert!(VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
    drop(guard);
    drop(
        GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert!(!VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());
}

#[test]
fn portable_copy_rejects_insufficient_headroom_before_copying() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let _guard = PortableCloneTestGuard::set(
        PortableCloneTestOptions {
            available_bytes: Some(0),
        },
        |_, _| panic!("headroom rejection must precede portable copy work"),
    );

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationInsufficientHeadroom {
            available: 0,
            required
        } if required > 0
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert!(VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());
}

#[test]
fn portable_copy_retains_committed_postvisibility_outcome() {
    let predecessor = GoldenPredecessor::copy();
    let _portable = PortableCloneTestGuard::set(PortableCloneTestOptions::default(), |_, _| Ok(()));
    let _atomic = AtomicWriteTestHookGuard::set(|stage, target| {
        if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
            && target.file_name().and_then(|name| name.to_str()) == Some("active-generation.json")
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    });

    let outcome = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(outcome.committed_migration_recovery().is_some());
    let writer = outcome.into_writer().unwrap();
    assert_ne!(
        writer.base_manifest().unwrap().generation_id().unwrap(),
        predecessor.generation_id()
    );
    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    assert!(!current.uses_allowlisted_predecessor_contract());
    assert_eq!(current.count_term("evidence").unwrap(), 3);
}

#[test]
fn portable_copy_rejects_unmanaged_files_without_publishing() {
    let predecessor = GoldenPredecessor::copy();
    fs::write(
        active_generation_path(predecessor.root()).join("untrusted.extra"),
        b"not authenticated by active Tantivy metadata",
    )
    .unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let _guard = PortableCloneTestGuard::set(PortableCloneTestOptions::default(), |_, _| Ok(()));

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationSourceTopology("unexpected directory entry")
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[test]
fn portable_clone_enforces_the_entry_cap_during_enumeration() {
    let predecessor = GoldenPredecessor::copy();
    fill_generation_past_migration_entry_cap(&active_generation_path(predecessor.root()));
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let _guard = PortableCloneTestGuard::set(PortableCloneTestOptions::default(), |_, _| Ok(()));

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationFileLimit {
            actual: 4_097,
            maximum: 4_096
        }
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[cfg(unix)]
#[test]
fn portable_copy_rejects_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let predecessor = GoldenPredecessor::copy();
    let generation = active_generation_path(predecessor.root());
    let active_file = fs::read_dir(&generation)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("store"))
        .unwrap();
    let escaped_file = predecessor.root().join("escaped-portable-segment.store");
    fs::rename(&active_file, &escaped_file).unwrap();
    symlink(&escaped_file, &active_file).unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let _guard = PortableCloneTestGuard::set(PortableCloneTestOptions::default(), |_, _| Ok(()));

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::ChecksumMismatch
            | IndexError::PredecessorMigrationSourceTopology(
                "symlink, reparse point, or remote-provider file in migration source"
            )
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[cfg(unix)]
#[test]
fn portable_copy_fails_closed_when_source_directory_name_is_swapped() {
    let predecessor = GoldenPredecessor::copy();
    let source = active_generation_path(predecessor.root());
    let moved = predecessor
        .root()
        .join("authenticated-source-held-by-handle");
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let source_for_hook = source.clone();
    let moved_for_hook = moved.clone();
    let mut swapped = false;
    let _guard =
        PortableCloneTestGuard::set(PortableCloneTestOptions::default(), move |stage, _| {
            if stage == PortableCloneStage::AfterSourceOpen && !swapped {
                fs::rename(&source_for_hook, &moved_for_hook)?;
                fs::create_dir(&source_for_hook)?;
                fs::write(
                    source_for_hook.join("replacement-sentinel"),
                    b"do not traverse",
                )?;
                swapped = true;
            }
            Ok(())
        });

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationSourceTopology(
            "migration directory changed after authentication"
        )
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        fs::read(source.join("replacement-sentinel")).unwrap(),
        b"do not traverse"
    );
    fs::remove_dir_all(&source).unwrap();
    fs::rename(moved, source).unwrap();
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[cfg(unix)]
#[test]
fn portable_cleanup_never_deletes_a_replacement_generation() {
    let predecessor = GoldenPredecessor::copy();
    let generations = predecessor.root().join(INDEX_GENERATIONS_DIRECTORY);
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let generations_for_hook = generations.clone();
    let replacement = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
    let replacement_for_hook = std::rc::Rc::clone(&replacement);
    let _guard = PortableCloneTestGuard::set(
        PortableCloneTestOptions::default(),
        move |stage, relative| {
            if stage == PortableCloneStage::BeforeCleanup {
                let candidate = generations_for_hook.join(relative);
                let orphan = generations_for_hook.join(format!(
                    "{}-authenticated-orphan",
                    relative.to_string_lossy()
                ));
                fs::rename(&candidate, &orphan)?;
                fs::create_dir(&candidate)?;
                fs::write(
                    candidate.join("replacement-sentinel"),
                    b"must survive cleanup",
                )?;
                *replacement_for_hook.borrow_mut() = Some(candidate);
            }
            Ok(())
        },
    );
    let _migration = MigrationTestHookGuard::set(|stage, _| {
        if stage == MigrationStage::BeforeCandidateCommit {
            return Err(io::Error::other("injected failure before cleanup").into());
        }
        Ok(())
    });

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::Io(ref error) if error.to_string() == "injected failure before cleanup"
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let replacement = replacement.borrow().clone().unwrap();
    assert_eq!(
        fs::read(replacement.join("replacement-sentinel")).unwrap(),
        b"must survive cleanup"
    );
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[test]
fn portable_copy_detects_growth_without_writing_past_authenticated_length() {
    let predecessor = GoldenPredecessor::copy();
    let source_generation = active_generation_path(predecessor.root());
    let source_file = fs::read_dir(&source_generation)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("store"))
        .unwrap();
    let source_name = source_file.file_name().unwrap().to_owned();
    let original_bytes = fs::metadata(&source_file).unwrap().len();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let source_for_hook = source_file.clone();
    let mut grew = false;
    let _guard = PortableCloneTestGuard::set(
        PortableCloneTestOptions::default(),
        move |stage, relative| {
            if stage == PortableCloneStage::AfterSourceOpen
                && relative == Path::new(&source_name)
                && !grew
            {
                use std::io::Write as _;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&source_for_hook)?
                    .write_all(b"growth-after-authentication")?;
                grew = true;
            }
            Ok(())
        },
    );

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationSourceTopology("source file grew while cloning")
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source_file)
        .unwrap()
        .set_len(original_bytes)
        .unwrap();
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

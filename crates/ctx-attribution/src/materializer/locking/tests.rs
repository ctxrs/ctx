use std::io::Write as _;

use super::*;

#[test]
fn progress_is_nonblocking_and_stale_counters_do_not_imply_an_active_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("graph");
    assert!(OperationLock::read_progress(&root).unwrap().is_none());
    assert!(!root.exists());
    prepare_private_root(&root).unwrap();
    let mut writer = OperationLock::acquire(&root).unwrap();
    writer.update_progress(
        |progress| {
            progress.phase = super::super::MaterializationPhase::Indexing;
            progress.total_sources = Some(4);
            progress.completed_sources = Some(2);
            progress.applied_changes = Some(100);
        },
        true,
    );
    let started = Instant::now();
    let progress = OperationLock::read_progress(&root).unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(progress.completed_sources, Some(2));
    assert_eq!(progress.applied_changes, Some(100));
    writer
        .progress_file
        .as_ref()
        .unwrap()
        .file()
        .set_len(0)
        .unwrap();
    let unavailable = OperationLock::read_progress(&root).unwrap().unwrap();
    assert_eq!(
        unavailable.phase,
        super::super::MaterializationPhase::SnapshotUnavailable
    );
    assert!(unavailable.completed_sources.is_none());
    assert!(unavailable.applied_changes.is_none());
    writer.update_progress(|_| {}, true);
    drop(writer);
    assert!(root.join(MATERIALIZER_PROGRESS_FILE).exists());
    assert!(OperationLock::read_progress(&root).unwrap().is_none());
}

#[cfg(unix)]
use std::os::unix::fs::symlink;

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn native_operation_lock_excludes_a_contender_and_releases_on_drop()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    prepare_private_root(&root)?;
    let owner = OperationLock::acquire(&root)?;

    assert!(matches!(
        OperationLock::acquire(&root),
        Err(SegmentMaterializerError::Busy)
    ));
    drop(owner);

    let successor = OperationLock::acquire(&root)?;
    successor.verify_identity()?;
    Ok(())
}

#[cfg(target_os = "windows")]
#[test]
fn windows_private_root_sync_policy_does_not_flush_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    prepare_private_root(&root)?;

    assert!(!NATIVE_DIRECTORY_SYNC_SUPPORTED);
    sync_private_root(&root)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn materializer_lock_rejects_symlink_hardlink_and_insecure_mode()
-> Result<(), Box<dyn std::error::Error>> {
    for attack in ["symlink", "hardlink", "mode"] {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join(attack);
        prepare_private_root(&root)?;
        let lock = root.join(MATERIALIZER_LOCK_FILE);
        let target = root.join("target");
        let mut target_file = create_private_file(&target)?;
        target_file.file_mut().write_all(b"target")?;
        target_file.file().sync_all()?;
        drop(target_file);
        match attack {
            "symlink" => symlink(&target, &lock)?,
            "hardlink" => std::fs::hard_link(&target, &lock)?,
            "mode" => {
                std::fs::copy(&target, &lock)?;
                std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644))?;
            }
            _ => unreachable!(),
        }
        assert!(OperationLock::acquire(&root).is_err(), "accepted {attack}");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn private_root_rejects_insecure_mode_and_symlink() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root");
    prepare_private_root(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;
    assert!(verify_private_root(&root).is_err());

    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let alias = directory.path().join("root-alias");
    symlink(&root, &alias)?;
    assert!(verify_private_root(&alias).is_err());
    Ok(())
}

#[test]
fn named_open_identity_substitution_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    prepare_private_root(&root)?;
    let path = root.join("candidate");
    let opened = create_private_file(&path)?;
    let displaced = root.join("displaced");
    std::fs::rename(&path, &displaced)?;
    let replacement = create_private_file(&path)?;
    drop(replacement);

    assert!(opened.verify_identity().is_err());
    Ok(())
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn held_operation_lock_detects_named_substitution() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    prepare_private_root(&root)?;
    let lock = OperationLock::acquire(&root)?;
    let lock_path = root.join(MATERIALIZER_LOCK_FILE);
    let displaced = root.join("displaced-lock");

    #[cfg(unix)]
    {
        std::fs::rename(&lock_path, &displaced)?;
        let replacement = create_private_file(&lock_path)?;
        drop(replacement);
        assert!(lock.verify_identity().is_err());
    }

    #[cfg(target_os = "windows")]
    {
        assert!(
            std::fs::rename(&lock_path, &displaced).is_err(),
            "a held lock handle must deny rename/delete sharing"
        );
        lock.verify_identity()?;
        drop(lock);
        std::fs::rename(&lock_path, &displaced)?;
        let replacement = create_private_file(&lock_path)?;
        drop(replacement);
        let successor = OperationLock::acquire(&root)?;
        successor.verify_identity()?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn private_file_open_rejects_hardlinks_and_insecure_mode() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    prepare_private_root(&root)?;
    let path = root.join("candidate");
    let file = create_private_file(&path)?;
    drop(file);
    let link = root.join("candidate-link");
    std::fs::hard_link(&path, &link)?;
    assert!(open_private_file(&path, false).is_err());
    assert!(open_private_file(&link, false).is_err());

    std::fs::remove_file(&link)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))?;
    assert!(open_private_file(&path, false).is_err());
    Ok(())
}

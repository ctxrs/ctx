use super::*;

#[cfg(unix)]
#[tokio::test]
async fn ensure_mount_applies_read_only_mode() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("notes.txt"), "hello\n").expect("write source file");

    ensure_mount(&target, &source, AttachmentMode::Ro)
        .await
        .expect("mount ro attachment");

    assert_tree_has_no_write_bits(&target);
    std::fs::write(source.join("source-writable.txt"), "still writable\n")
        .expect("ro attachment mount should not mutate source writability");
}

#[test]
fn ro_copy_applies_read_only_to_temp_before_final_rename() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("notes.txt"), "hello\n").expect("write source file");

    let mut chmod_path = None;
    let err = copy_path_recursive_read_only_atomic_with(
        &source,
        &target,
        |path| {
            chmod_path = Some(path.to_path_buf());
            assert_ne!(
                path,
                target.as_path(),
                "chmod must happen before final rename"
            );
            assert!(path.exists(), "staged copy must exist before chmod");
            anyhow::bail!("injected read-only failure");
        },
        |_| Ok(()),
    )
    .expect_err("injected chmod failure should fail the copy");

    assert!(format!("{err:#}").contains("injected read-only failure"));
    let chmod_path = chmod_path.expect("chmod hook should be called");
    assert!(
        !target.exists(),
        "failed read-only chmod must not leave final mount target visible"
    );
    assert!(
        !chmod_path.exists(),
        "failed read-only chmod must clean staged temp copy"
    );
    let leftovers = std::fs::read_dir(temp.path())
        .expect("read temp root")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name == "target" || name.starts_with(".target.copy-tmp.")),
        "failed read-only chmod left target or staged copy entries: {leftovers:?}"
    );
}

#[test]
fn ro_copy_restores_existing_target_when_final_rename_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("notes.txt"), "replacement\n").expect("write source file");
    std::fs::create_dir_all(&target).expect("create target");
    std::fs::write(target.join("existing.txt"), "existing\n").expect("write existing target");
    #[cfg(unix)]
    apply_read_only_mode_recursive(&target).expect("make existing target read-only");
    let mut after_rename_called = false;

    let err = copy_path_recursive_read_only_atomic_with(
        &source,
        &target,
        |path| {
            std::fs::remove_dir_all(path).expect("remove staged copy before final rename");
            Ok(())
        },
        |_| {
            after_rename_called = true;
            Ok(())
        },
    )
    .expect_err("missing staged copy should fail final rename");

    assert!(!format!("{err:#}").is_empty());
    assert!(!after_rename_called, "post-rename chmod must not run");
    assert_eq!(
        std::fs::read_to_string(target.join("existing.txt")).unwrap(),
        "existing\n",
        "failed final rename must restore the previous mount target"
    );
    #[cfg(unix)]
    assert_tree_has_no_write_bits(&target);
    let leftovers = std::fs::read_dir(temp.path())
        .expect("read temp root")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers.iter().any(|name| {
            name.starts_with(".target.copy-tmp.") || name.starts_with(".target.copy-old.")
        }),
        "failed final rename left staged or backup entries: {leftovers:?}"
    );
    #[cfg(unix)]
    clear_read_only_mode(&target).expect("restore target writability for temp cleanup");
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_mount_ro_copy_rejects_file_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("guide.md"), "guide\n").expect("write source file");
    std::os::unix::fs::symlink(source.join("guide.md"), source.join("guide-link"))
        .expect("symlink guide");

    let err = ensure_mount(&target, &source, AttachmentMode::Ro)
        .await
        .expect_err("file symlink should fail closed for ro copies");

    assert!(format!("{err:#}").contains("refuses symlink"));
    assert!(
        !target.exists(),
        "failed ro copy must not leave a partial writable mount tree"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_mount_ro_copy_rejects_directory_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(source.join("real-dir")).expect("create source dir");
    std::os::unix::fs::symlink(source.join("real-dir"), source.join("dir-link"))
        .expect("symlink dir");

    let err = ensure_mount(&target, &source, AttachmentMode::Ro)
        .await
        .expect_err("directory symlink should fail closed for ro copies");

    assert!(format!("{err:#}").contains("refuses symlink"));
    assert!(
        !target.exists(),
        "failed ro copy must not leave a partial writable mount tree"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_mount_ro_copy_rejects_external_file_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let outside = temp.path().join("outside-secret.txt");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(&outside, "outside\n").expect("write outside file");
    std::os::unix::fs::symlink(&outside, source.join("secret-link")).expect("symlink outside file");

    let err = ensure_mount(&target, &source, AttachmentMode::Ro)
        .await
        .expect_err("external file symlink should fail closed for ro copies");

    assert!(format!("{err:#}").contains("refuses symlink"));
    assert!(
        !target.exists(),
        "failed ro copy must not leave a partial writable mount tree"
    );
}

#[tokio::test]
async fn ensure_mount_switches_from_ro_copy_to_rw_mount() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("notes.txt"), "hello\n").expect("write source file");

    ensure_mount(&target, &source, AttachmentMode::Ro)
        .await
        .expect("mount ro attachment");
    ensure_mount(&target, &source, AttachmentMode::Rw)
        .await
        .expect("remount rw attachment");

    std::fs::write(target.join("notes.txt"), "mutated\n")
        .expect("rw attachment remount should allow writes");
}

#[tokio::test]
async fn remove_mount_path_deletes_read_only_mount_copy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("notes.txt"), "hello\n").expect("write source file");

    ensure_mount(&target, &source, AttachmentMode::Ro)
        .await
        .expect("mount ro attachment");
    remove_mount_path(&target)
        .await
        .expect("remove ro attachment mount");

    assert!(!target.exists(), "ro attachment mount should be removed");
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_mount_in_worktree_rejects_symlinked_ctx_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    let outside = temp.path().join("outside");
    let source = temp.path().join("source");
    std::fs::create_dir_all(&worktree).expect("create worktree");
    std::fs::create_dir_all(&outside).expect("create outside");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("notes.txt"), "hello\n").expect("write source file");
    std::os::unix::fs::symlink(&outside, worktree.join(".ctx")).expect("symlink .ctx");

    let err = ensure_mount_in_worktree(
        &worktree,
        Path::new(".ctx/attachments/docs/docs"),
        &source,
        AttachmentMode::Ro,
    )
    .await
    .expect_err("symlinked .ctx parent should fail closed");

    assert!(format!("{err:#}").contains("must not be a symlink"));
    assert!(
        !outside.join("attachments").exists(),
        "mount creation must not follow .ctx symlink outside the worktree"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn remove_mount_path_in_worktree_rejects_symlinked_attachments_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree = temp.path().join("worktree");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(worktree.join(".ctx")).expect("create .ctx");
    std::fs::create_dir_all(outside.join("docs").join("docs")).expect("create outside mount");
    std::fs::write(
        outside.join("docs").join("docs").join("notes.txt"),
        "keep\n",
    )
    .expect("write outside file");
    std::os::unix::fs::symlink(&outside, worktree.join(".ctx").join("attachments"))
        .expect("symlink attachments");

    let target = worktree.join(".ctx/attachments/docs/docs");
    let err = remove_mount_path_in_worktree(&worktree, &target)
        .await
        .expect_err("symlinked .ctx/attachments parent should fail closed");

    assert!(format!("{err:#}").contains("must not be a symlink"));
    assert!(
        outside.join("docs").join("docs").join("notes.txt").exists(),
        "mount cleanup must not delete through .ctx/attachments symlink"
    );
}

#[tokio::test]
async fn ensure_mount_leaves_rw_mounts_writable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    std::fs::create_dir_all(&source).expect("create source");
    std::fs::write(source.join("notes.txt"), "hello\n").expect("write source file");

    ensure_mount(&target, &source, AttachmentMode::Rw)
        .await
        .expect("mount rw attachment");

    std::fs::write(target.join("notes.txt"), "mutated\n")
        .expect("rw attachment mount should allow writes");
}

#[cfg(unix)]
fn assert_tree_has_no_write_bits(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path).expect("read path metadata");
    assert_eq!(
        metadata.permissions().mode() & 0o222,
        0,
        "path retained write bits: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).expect("read directory") {
            assert_tree_has_no_write_bits(&entry.expect("read entry").path());
        }
    }
}

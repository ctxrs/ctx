use crate::validate_mount_path_in_worktree;

use super::avf::{avf_import_dir_script, avf_import_file_script, avf_remove_mount_path_script};
use super::native::{
    container_import_dir_script, container_mount_script, container_remove_mount_path_script,
};
use super::{
    resolve_attachment_source_path, sandbox_mount_parent_chain_ensure_test_script,
    validate_read_only_attachment_import_tree, AttachmentSourceSymlinkPolicy,
};

#[tokio::test]
async fn resolve_attachment_source_path_rejects_parent_traversal() {
    let root = tempfile::tempdir().unwrap();
    let err = resolve_attachment_source_path(
        root.path(),
        Some("../escape.txt"),
        AttachmentSourceSymlinkPolicy::AllowInternal,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("attachment source not found"));
}

#[tokio::test]
async fn resolve_attachment_source_path_rejects_symlink_escape() {
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("escape.txt"), b"escape").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();

        let err = resolve_attachment_source_path(
            root.path(),
            Some("link/escape.txt"),
            AttachmentSourceSymlinkPolicy::AllowInternal,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("escapes the materialized root"));
    }
}

#[tokio::test]
async fn resolve_attachment_source_path_rejects_nested_symlink_escape() {
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("escape.txt"), b"escape").unwrap();
        std::os::unix::fs::symlink(outside.path().join("escape.txt"), docs.join("leak")).unwrap();

        let err = resolve_attachment_source_path(
            root.path(),
            None,
            AttachmentSourceSymlinkPolicy::AllowInternal,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("escapes the materialized root"));
    }
}

#[tokio::test]
async fn resolve_attachment_source_path_allows_internal_symlinks() {
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("guide.md"), b"guide").unwrap();
        std::os::unix::fs::symlink("guide.md", docs.join("guide-link")).unwrap();

        let resolved = resolve_attachment_source_path(
            root.path(),
            None,
            AttachmentSourceSymlinkPolicy::AllowInternal,
        )
        .await
        .unwrap();
        assert_eq!(resolved, root.path().canonicalize().unwrap());
    }
}

#[tokio::test]
async fn resolve_attachment_source_path_rejects_internal_symlinks_for_read_only_copy() {
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("guide.md"), b"guide").unwrap();
        std::os::unix::fs::symlink("guide.md", docs.join("guide-link")).unwrap();

        let err = resolve_attachment_source_path(
            root.path(),
            None,
            AttachmentSourceSymlinkPolicy::Reject,
        )
        .await
        .unwrap_err();

        assert!(format!("{err:#}").contains("refuses symlink"));
    }
}

#[tokio::test]
async fn native_read_only_import_tree_rejects_symlink_outside_selected_subpath() {
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("guide.md"), b"guide").unwrap();
        std::fs::write(root.path().join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink("secret.txt", root.path().join("secret-link")).unwrap();

        let selected = resolve_attachment_source_path(
            root.path(),
            Some("docs"),
            AttachmentSourceSymlinkPolicy::Reject,
        )
        .await
        .unwrap();
        assert_eq!(selected, docs.canonicalize().unwrap());

        let err = validate_read_only_attachment_import_tree(root.path())
            .await
            .unwrap_err();

        assert!(format!("{err:#}").contains("refuses symlink"));
    }
}

#[tokio::test]
async fn sandbox_mount_validation_rejects_symlinked_ctx_parent() {
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, worktree.join(".ctx")).unwrap();

        let target = worktree.join(".ctx/attachments/docs/docs");
        let err = validate_mount_path_in_worktree(&worktree, &target)
            .expect_err("sandbox mount validation should reject symlinked .ctx");

        assert!(format!("{err:#}").contains("must not be a symlink"));
    }
}

#[tokio::test]
async fn sandbox_mount_validation_rejects_symlinked_attachments_parent() {
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(worktree.join(".ctx")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, worktree.join(".ctx").join("attachments")).unwrap();

        let target = worktree.join(".ctx/attachments/docs/docs");
        let err = validate_mount_path_in_worktree(&worktree, &target)
            .expect_err("sandbox mount validation should reject symlinked attachments parent");

        assert!(format!("{err:#}").contains("must not be a symlink"));
    }
}

#[tokio::test]
async fn sandbox_mount_validation_rejects_symlinked_docs_parent() {
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(worktree.join(".ctx/attachments")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, worktree.join(".ctx/attachments/docs")).unwrap();

        let target = worktree.join(".ctx/attachments/docs/docs");
        let err = validate_mount_path_in_worktree(&worktree, &target)
            .expect_err("sandbox mount validation should reject symlinked docs parent");

        assert!(format!("{err:#}").contains("must not be a symlink"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sandbox_guest_parent_chain_script_rejects_symlinked_ctx_parent() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, worktree.join(".ctx")).unwrap();

    let target = worktree.join(".ctx/attachments/docs/docs");
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(sandbox_mount_parent_chain_ensure_test_script())
        .arg("--")
        .arg(&worktree)
        .arg(&target)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("must not be a symlink"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !outside.join("attachments").exists(),
        "validation must not create through a symlinked guest parent"
    );

    let relative_output = std::process::Command::new("sh")
        .current_dir(&worktree)
        .arg("-c")
        .arg(sandbox_mount_parent_chain_ensure_test_script())
        .arg("--")
        .arg(".")
        .arg("./.ctx/attachments/docs/docs")
        .output()
        .unwrap();

    assert!(!relative_output.status.success());
    assert!(
        String::from_utf8_lossy(&relative_output.stderr).contains("must not be a symlink"),
        "stderr: {}",
        String::from_utf8_lossy(&relative_output.stderr)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sandbox_guest_parent_chain_script_creates_and_verifies_missing_safe_parents() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();

    let target = worktree.join(".ctx/attachments/docs/docs");
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(sandbox_mount_parent_chain_ensure_test_script())
        .arg("--")
        .arg(&worktree)
        .arg(&target)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        worktree.join(".ctx/attachments/docs").is_dir(),
        "parent chain should be created one component at a time"
    );
}

#[test]
fn sandbox_guest_mount_scripts_create_and_verify_without_mkdir_p_preflight() {
    let scripts = [
        container_import_dir_script(),
        container_mount_script(),
        container_remove_mount_path_script(),
        avf_import_dir_script(),
        avf_import_file_script(),
        avf_remove_mount_path_script(),
    ];
    for script in scripts {
        assert!(script.contains("set -eu"));
        assert!(script.contains("ensure_mount_parent_chain"));
        assert!(script.contains("mkdir \"$current\""));
        assert!(
            !script.contains("mkdir -p"),
            "guest mount mutation scripts must not use split validation plus mkdir -p"
        );
    }

    let staging_scripts = [
        container_import_dir_script(),
        container_mount_script(),
        avf_import_dir_script(),
        avf_import_file_script(),
    ];
    for script in staging_scripts {
        assert!(script.contains("cleanup_stage"));
        assert!(script.contains("trap cleanup_stage EXIT"));
        assert!(script.contains("mktemp -d"));
        assert!(
            script.contains("mv -- \"$temp\" \"$target\"")
                || script.contains("mv -- \"$temp\" \"$dest\"")
        );
    }

    let avf_dir_script = avf_import_dir_script();
    assert!(
        !avf_dir_script.contains("tar -C \"$target\""),
        "AVF directory import must extract into staged temp, not final target"
    );
    let container_import_script = container_import_dir_script();
    assert!(
        !container_import_script.contains("tar -C \"$dest\""),
        "native container materialization import must extract into staged temp, not final dest"
    );
    let avf_file_script = avf_import_file_script();
    assert!(
        !avf_file_script.contains("cat > \"$target\""),
        "AVF file import must write into staged temp, not final target"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_container_import_script_cleans_temp_and_leaves_no_dest_on_tar_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("container-root");
    let dest = root.join("attachments/attachment/revision");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();

    let fake_tar = fake_bin.join("tar");
    std::fs::write(
        &fake_tar,
        "#!/bin/sh\nmkdir -p \"$2\"\nprintf partial > \"$2/partial.txt\"\nexit 7\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_tar).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_tar, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .arg("-c")
        .arg(container_import_dir_script())
        .arg("--")
        .arg(&root)
        .arg(&dest)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "native container materialization import must fail when tar extraction fails"
    );
    assert!(
        !dest.exists(),
        "failed native container import must not leave a final materialized root"
    );
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "failed native container import left staged or partial entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_container_import_script_preserves_existing_dest_on_tar_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("container-root");
    let dest = root.join("attachments/attachment/revision");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("existing.txt"), b"existing").unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();

    let fake_tar = fake_bin.join("tar");
    std::fs::write(
        &fake_tar,
        "#!/bin/sh\nmkdir -p \"$2\"\nprintf partial > \"$2/partial.txt\"\nexit 7\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_tar).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_tar, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .arg("-c")
        .arg(container_import_dir_script())
        .arg("--")
        .arg(&root)
        .arg(&dest)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "native container import must fail when staged tar extraction fails"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("existing.txt")).unwrap(),
        "existing",
        "failed native container import refresh must preserve existing materialization"
    );
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers.iter().any(|name| name.contains("import-tmp")),
        "failed native container import left staged entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_container_import_script_restores_existing_dest_on_final_mv_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("container-root");
    let dest = root.join("attachments/attachment/revision");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("existing.txt"), b"existing").unwrap();
    set_tree_read_only(&dest);
    std::fs::create_dir_all(&fake_bin).unwrap();
    write_fake_mv_failing_payload(&fake_bin);

    let fake_tar = fake_bin.join("tar");
    std::fs::write(
        &fake_tar,
        "#!/bin/sh\nmkdir -p \"$2\"\nprintf replacement > \"$2/replacement.txt\"\nexit 0\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_tar).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_tar, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .arg("-c")
        .arg(container_import_dir_script())
        .arg("--")
        .arg(&root)
        .arg(&dest)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "native container import must fail when final staged install fails"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("existing.txt")).unwrap(),
        "existing",
        "failed native container final install must restore existing materialization"
    );
    assert_tree_read_only(&dest);
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.contains("import-tmp") || name.contains(".old.")),
        "failed native container final install left staged entries: {leftovers:?}"
    );
    set_tree_writable(&dest);
}

#[cfg(unix)]
#[tokio::test]
async fn native_ro_mount_script_propagates_copy_failure_before_chmod_success() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let source = temp.path().join("source");
    let target = worktree.join(".ctx/attachments/docs/docs");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    std::fs::write(source.join("notes.txt"), b"notes").unwrap();

    let fake_cp = fake_bin.join("cp");
    std::fs::write(
        &fake_cp,
        "#!/bin/sh\nmkdir -p \"$4\"\nprintf partial > \"$4/partial.txt\"\nexit 7\n",
    )
    .unwrap();
    let mut cp_permissions = std::fs::metadata(&fake_cp).unwrap().permissions();
    cp_permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_cp, cp_permissions).unwrap();

    let fake_chmod = fake_bin.join("chmod");
    std::fs::write(&fake_chmod, "#!/bin/sh\nexit 0\n").unwrap();
    let mut chmod_permissions = std::fs::metadata(&fake_chmod).unwrap().permissions();
    chmod_permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_chmod, chmod_permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .env("PATH", path)
        .arg("-c")
        .arg(container_mount_script())
        .arg("--")
        .arg(&worktree)
        .arg(&target)
        .arg(&source)
        .arg("ro")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "ro mount script must fail when cp fails even if chmod succeeds"
    );
    assert!(
        !target.exists(),
        "failed native ro copy must not leave a final mount target"
    );
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "failed native ro copy left staged or partial entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_ro_mount_script_restores_existing_target_on_final_mv_failure() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let source = temp.path().join("source");
    let target = worktree.join(".ctx/attachments/docs/docs");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    std::fs::write(source.join("notes.txt"), b"replacement").unwrap();
    std::fs::write(target.join("existing.txt"), b"existing").unwrap();
    set_tree_read_only(&target);
    write_fake_mv_failing_payload(&fake_bin);

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .env("PATH", path)
        .arg("-c")
        .arg(container_mount_script())
        .arg("--")
        .arg(&worktree)
        .arg(&target)
        .arg(&source)
        .arg("ro")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "native ro mount refresh must fail when final staged install fails"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("existing.txt")).unwrap(),
        "existing",
        "failed native ro final install must restore existing mount target"
    );
    assert_tree_read_only(&target);
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.contains(".tmp.") || name.contains(".old.")),
        "failed native ro final install left staged entries: {leftovers:?}"
    );
    set_tree_writable(&target);
}

#[cfg(unix)]
#[tokio::test]
async fn native_ro_mount_script_preserves_existing_target_on_symlink_rejection() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let source = temp.path().join("source");
    let target = worktree.join(".ctx/attachments/docs/docs");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(source.join("notes.txt"), b"notes").unwrap();
    std::os::unix::fs::symlink("notes.txt", source.join("notes-link")).unwrap();
    std::fs::write(target.join("existing.txt"), b"existing").unwrap();

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(container_mount_script())
        .arg("--")
        .arg(&worktree)
        .arg(&target)
        .arg(&source)
        .arg("ro")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "native ro mount refresh must reject symlinked source"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("existing.txt")).unwrap(),
        "existing",
        "failed native ro refresh must preserve existing mount target"
    );
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers.iter().any(|name| name.contains(".tmp.")),
        "failed native ro refresh left staged entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn avf_dir_import_script_cleans_temp_and_leaves_no_target_on_tar_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let target = worktree.join(".ctx/attachments/docs/docs");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();

    let fake_tar = fake_bin.join("tar");
    std::fs::write(
        &fake_tar,
        "#!/bin/sh\nmkdir -p \"$2\"\nprintf partial > \"$2/partial.txt\"\nexit 7\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_tar).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_tar, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .current_dir(&worktree)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .arg("-c")
        .arg(avf_import_dir_script())
        .arg("--")
        .arg("./.ctx/attachments/docs/docs")
        .arg("ro")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "AVF directory import must fail when tar extraction fails"
    );
    assert!(
        !target.exists(),
        "failed AVF directory import must not leave a final mount target"
    );
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "failed AVF directory import left staged or partial entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn avf_dir_import_script_preserves_existing_target_on_tar_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let target = worktree.join(".ctx/attachments/docs/docs");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("existing.txt"), b"existing").unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();

    let fake_tar = fake_bin.join("tar");
    std::fs::write(
        &fake_tar,
        "#!/bin/sh\nmkdir -p \"$2\"\nprintf partial > \"$2/partial.txt\"\nexit 7\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_tar).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_tar, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .current_dir(&worktree)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .arg("-c")
        .arg(avf_import_dir_script())
        .arg("--")
        .arg("./.ctx/attachments/docs/docs")
        .arg("ro")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "AVF directory import refresh must fail when staged tar extraction fails"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("existing.txt")).unwrap(),
        "existing",
        "failed AVF directory refresh must preserve existing mount target"
    );
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers.iter().any(|name| name.contains(".tmp.")),
        "failed AVF directory refresh left staged entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn avf_dir_import_script_restores_existing_target_on_final_mv_failure() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let target = worktree.join(".ctx/attachments/docs/docs");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("existing.txt"), b"existing").unwrap();
    set_tree_read_only(&target);
    std::fs::create_dir_all(&fake_bin).unwrap();
    write_fake_mv_failing_payload(&fake_bin);

    let fake_tar = fake_bin.join("tar");
    std::fs::write(
        &fake_tar,
        "#!/bin/sh\nmkdir -p \"$2\"\nprintf replacement > \"$2/replacement.txt\"\nexit 0\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_tar).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_tar, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("sh")
        .current_dir(&worktree)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .arg("-c")
        .arg(avf_import_dir_script())
        .arg("--")
        .arg("./.ctx/attachments/docs/docs")
        .arg("ro")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "AVF directory import refresh must fail when final staged install fails"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("existing.txt")).unwrap(),
        "existing",
        "failed AVF directory final install must restore existing mount target"
    );
    assert_tree_read_only(&target);
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.contains(".tmp.") || name.contains(".old.")),
        "failed AVF directory final install left staged entries: {leftovers:?}"
    );
    set_tree_writable(&target);
}

#[cfg(unix)]
#[tokio::test]
async fn avf_file_import_script_cleans_temp_and_leaves_no_target_on_chmod_failure() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let target = worktree.join(".ctx/attachments/docs/readme.md");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();

    let fake_chmod = fake_bin.join("chmod");
    std::fs::write(&fake_chmod, "#!/bin/sh\nexit 9\n").unwrap();
    let mut permissions = std::fs::metadata(&fake_chmod).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_chmod, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = std::process::Command::new("sh")
        .current_dir(&worktree)
        .env("PATH", path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .arg("-c")
        .arg(avf_import_file_script())
        .arg("--")
        .arg("./.ctx/attachments/docs/readme.md")
        .arg("ro")
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"payload").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        !output.status.success(),
        "AVF file import must fail when read-only chmod fails"
    );
    assert!(
        !target.exists(),
        "failed AVF file import must not leave a final mount target"
    );
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "failed AVF file import left staged or partial entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn avf_file_import_script_preserves_existing_target_on_chmod_failure() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let target = worktree.join(".ctx/attachments/docs/readme.md");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"existing").unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();

    let fake_chmod = fake_bin.join("chmod");
    std::fs::write(&fake_chmod, "#!/bin/sh\nexit 9\n").unwrap();
    let mut permissions = std::fs::metadata(&fake_chmod).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_chmod, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = std::process::Command::new("sh")
        .current_dir(&worktree)
        .env("PATH", path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .arg("-c")
        .arg(avf_import_file_script())
        .arg("--")
        .arg("./.ctx/attachments/docs/readme.md")
        .arg("ro")
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"payload").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        !output.status.success(),
        "AVF file import refresh must fail when read-only chmod fails"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "existing",
        "failed AVF file refresh must preserve existing mount target"
    );
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers.iter().any(|name| name.contains(".tmp.")),
        "failed AVF file refresh left staged entries: {leftovers:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn avf_file_import_script_restores_existing_target_on_final_mv_failure() {
    use std::io::Write;

    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    let target = worktree.join(".ctx/attachments/docs/readme.md");
    let fake_bin = temp.path().join("bin");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"existing").unwrap();
    set_tree_read_only(&target);
    std::fs::create_dir_all(&fake_bin).unwrap();
    write_fake_mv_failing_payload(&fake_bin);

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = std::process::Command::new("sh")
        .current_dir(&worktree)
        .env("PATH", path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .arg("-c")
        .arg(avf_import_file_script())
        .arg("--")
        .arg("./.ctx/attachments/docs/readme.md")
        .arg("ro")
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"payload").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        !output.status.success(),
        "AVF file import refresh must fail when final staged install fails"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "existing",
        "failed AVF file final install must restore existing mount target"
    );
    assert_tree_read_only(&target);
    let leftovers = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.contains(".tmp.") || name.contains(".old.")),
        "failed AVF file final install left staged entries: {leftovers:?}"
    );
    set_tree_writable(&target);
}

#[cfg(unix)]
fn write_fake_mv_failing_payload(fake_bin: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let fake_mv = fake_bin.join("mv");
    std::fs::write(
        &fake_mv,
        "#!/bin/sh\nsrc=\"$1\"\nif [ \"$src\" = \"--\" ]; then\n  src=\"$2\"\nfi\ncase \"$src\" in\n  */payload) exit 7 ;;\nesac\nexec /bin/mv \"$@\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_mv).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_mv, permissions).unwrap();
}

#[cfg(unix)]
fn set_tree_read_only(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            set_tree_read_only(&entry.unwrap().path());
        }
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() & !0o222);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn set_tree_writable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path).unwrap();
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    std::fs::set_permissions(path, permissions).unwrap();
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            set_tree_writable(&entry.unwrap().path());
        }
    }
}

#[cfg(unix)]
fn assert_tree_read_only(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert_eq!(
        metadata.permissions().mode() & 0o222,
        0,
        "path retained write bits: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            assert_tree_read_only(&entry.unwrap().path());
        }
    }
}

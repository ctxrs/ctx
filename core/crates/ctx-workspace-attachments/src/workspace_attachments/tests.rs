use super::materialized_install::{install_materialized_temp, unique_materialized_temp_path};
use super::materialized_paths::{default_mount_relpath, ensure_materialized_revision_parent};
use super::reference_repo::materialize_reference_repo;
use super::{
    materialized_path_for_attachment, materialized_root_for_attachment,
    normalize_attachment_config, remove_materialized_root_if_exists, revision_key,
    sanitize_attachment_subpath, sanitize_mount_relpath, validate_materialized_path,
    AttachmentConfig,
};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{
    AttachmentMode, AttachmentUpdatePolicy, WorkspaceAttachmentKind, WorkspaceAttachmentStatus,
};

#[test]
fn default_mount_relpath_uses_kind_specific_roots() {
    assert_eq!(
        default_mount_relpath(&WorkspaceAttachmentKind::ReferenceRepo, "My Docs"),
        ".ctx/attachments/refs/my-docs"
    );
    assert_eq!(
        default_mount_relpath(&WorkspaceAttachmentKind::DocMirror, "API Guide"),
        ".ctx/attachments/docs/api-guide"
    );
}

#[test]
fn sanitize_mount_relpath_rejects_invalid_paths() {
    assert!(sanitize_mount_relpath(".ctx/attachments/docs/api-guide").is_ok());
    assert!(sanitize_mount_relpath("").is_err());
    assert!(sanitize_mount_relpath(".").is_err());
    assert!(sanitize_mount_relpath("./x").is_err());
    assert!(sanitize_mount_relpath("x/.").is_err());
    assert!(sanitize_mount_relpath("x//y").is_err());
    assert!(sanitize_mount_relpath("/absolute/path").is_err());
    assert!(sanitize_mount_relpath("../escape").is_err());
    assert!(sanitize_mount_relpath("safe\\escape").is_err());
}

#[test]
fn sanitize_attachment_subpath_rejects_escape_paths() {
    assert!(sanitize_attachment_subpath("guide/index.md").is_ok());
    assert!(sanitize_attachment_subpath("").is_err());
    assert!(sanitize_attachment_subpath("/absolute/path").is_err());
    assert!(sanitize_attachment_subpath("../escape").is_err());
    assert!(sanitize_attachment_subpath("guide/../../escape").is_err());
}

#[test]
fn normalize_attachment_config_preserves_existing_identity() {
    let workspace_id = WorkspaceId::new();
    let existing = normalize_attachment_config(
        workspace_id,
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "https://example.com/repo.git".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: Some(AttachmentMode::Ro),
            update_policy: Some(AttachmentUpdatePolicy::Manual),
        },
        None,
    )
    .unwrap();

    let updated = normalize_attachment_config(
        workspace_id,
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "https://example.com/repo.git".to_string(),
            revision: Some("main".to_string()),
            subpath: Some("guide".to_string()),
            mount_relpath: Some(".ctx/attachments/refs/docs".to_string()),
            mode: Some(AttachmentMode::Ro),
            update_policy: Some(AttachmentUpdatePolicy::OnOpen),
        },
        Some(existing.clone()),
    )
    .unwrap();

    assert_eq!(updated.id, existing.id);
    assert_eq!(updated.created_at, existing.created_at);
    assert_eq!(updated.status, WorkspaceAttachmentStatus::Pending);
    assert_eq!(updated.mount_relpath, ".ctx/attachments/refs/docs");
    assert_eq!(updated.revision.as_deref(), Some("main"));
    assert_eq!(updated.subpath.as_deref(), Some("guide"));
    assert_eq!(updated.update_policy, AttachmentUpdatePolicy::OnOpen);
}

#[test]
fn revision_key_defaults_and_sanitizes() {
    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::DocMirror,
            name: "Docs".to_string(),
            source: "https://example.com".to_string(),
            revision: Some("Feature/Branch".to_string()),
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .unwrap();
    assert_eq!(revision_key(&attachment), "feature-branch");
}

#[test]
fn normalize_attachment_config_rejects_blank_source() {
    let err = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "   ".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .expect_err("blank attachment source should fail");

    assert!(format!("{err:#}").contains("source must not be empty"));
}

#[test]
fn normalize_reference_repo_local_source_requires_absolute_path() {
    let err = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "../references/docs".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .expect_err("relative local reference repo source should fail");

    assert!(format!("{err:#}").contains("absolute path or repository URL"));
}

#[test]
fn normalize_attachment_config_rejects_invalid_mount_relpath() {
    let err = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "https://example.com/repo.git".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: Some(".".to_string()),
            mode: None,
            update_policy: None,
        },
        None,
    )
    .expect_err("dot mount_relpath should fail");

    assert!(format!("{err:#}").contains("mount_relpath"));
}

#[test]
fn normalize_reference_repo_accepts_scp_style_ssh_urls() {
    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "git@github.com:openai/ctx.git".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .expect("scp-style ssh source should remain supported");

    assert_eq!(attachment.source, "git@github.com:openai/ctx.git");
}

#[test]
fn normalize_reference_repo_accepts_host_alias_scp_urls() {
    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "corp-git:team/repo.git".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .expect("host-alias scp source should remain supported");

    assert_eq!(attachment.source, "corp-git:team/repo.git");
}

#[test]
fn normalize_doc_mirror_rejects_local_script_source() {
    let err = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::DocMirror,
            name: "Docs".to_string(),
            source: ".ctx/scripts/docs.py".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .expect_err("local doc mirror scripts should fail closed");

    assert!(format!("{err:#}").contains("http(s) URL"));
    assert!(format!("{err:#}").contains("not supported"));
}

#[test]
fn normalize_doc_mirror_rejects_rw_mode() {
    let err = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::DocMirror,
            name: "Docs".to_string(),
            source: "https://example.com/docs".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: Some(AttachmentMode::Rw),
            update_policy: None,
        },
        None,
    )
    .expect_err("rw doc mirror should fail closed");

    assert!(format!("{err:#}").contains("read-only"));
}

#[cfg(unix)]
#[tokio::test]
async fn materialized_parent_creation_rejects_symlinked_attachment_store_parent() {
    let data_root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "https://example.com/repo.git".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .unwrap();
    std::fs::create_dir_all(data_root.path().join("attachments/reference-repos")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        data_root
            .path()
            .join("attachments/reference-repos/checkouts"),
    )
    .unwrap();

    let err = ensure_materialized_revision_parent(data_root.path(), &attachment)
        .await
        .expect_err("symlinked materialization parent should fail");

    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[cfg(unix)]
#[tokio::test]
async fn materialized_root_removal_rejects_symlinked_root_without_touching_target() {
    let data_root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep.txt"), b"keep").unwrap();
    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "https://example.com/repo.git".to_string(),
            revision: None,
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .unwrap();
    let root = materialized_root_for_attachment(data_root.path(), &attachment);
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &root).unwrap();

    let err = remove_materialized_root_if_exists(data_root.path(), &attachment)
        .await
        .expect_err("symlinked materialization root should fail");

    assert!(format!("{err:#}").contains("must not be a symlink"));
    assert!(
        outside.path().join("keep.txt").exists(),
        "cleanup must not delete through materialized root symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn materialized_path_validation_rejects_symlinked_revision_path() {
    let data_root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "https://example.com/repo.git".to_string(),
            revision: Some("main".to_string()),
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .unwrap();
    let root = materialized_root_for_attachment(data_root.path(), &attachment);
    let revision_path = root.join(revision_key(&attachment));
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(outside.path(), &revision_path).unwrap();

    let err = validate_materialized_path(data_root.path(), &attachment)
        .await
        .expect_err("symlinked revision path should fail");

    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[tokio::test]
async fn reference_repo_materialization_cleans_temp_and_final_on_checkout_failure() {
    let data_root = tempfile::tempdir().unwrap();
    let (_source_parent, source) = create_reference_repo();

    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: source.to_string_lossy().to_string(),
            revision: Some("deadbee".to_string()),
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .unwrap();

    let err = materialize_reference_repo(data_root.path(), &attachment, true)
        .await
        .expect_err("invalid checkout revision should fail");

    assert!(format!("{err:#}").contains("git fetch failed"));
    let dest = materialized_path_for_attachment(data_root.path(), &attachment);
    assert!(
        !dest.exists(),
        "failed reference repo materialization must not leave final revision path"
    );
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.contains("materialize-tmp")),
        "failed reference repo materialization left temp entries: {leftovers:?}"
    );
}

#[tokio::test]
async fn reference_repo_refresh_failure_preserves_existing_materialization() {
    let data_root = tempfile::tempdir().unwrap();
    let (_source_parent, source) = create_reference_repo();

    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: source.to_string_lossy().to_string(),
            revision: Some("deadbee".to_string()),
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .unwrap();
    let dest = materialized_path_for_attachment(data_root.path(), &attachment);
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("existing.txt"), "existing\n").unwrap();

    let err = materialize_reference_repo(data_root.path(), &attachment, true)
        .await
        .expect_err("invalid checkout revision should fail refresh");

    assert!(format!("{err:#}").contains("git fetch failed"));
    assert_eq!(
        std::fs::read_to_string(dest.join("existing.txt")).unwrap(),
        "existing\n",
        "failed refresh must preserve the previous materialization"
    );
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.contains("materialize-tmp")),
        "failed reference repo refresh left temp entries: {leftovers:?}"
    );
}

#[tokio::test]
async fn materialized_temp_install_restores_existing_materialization_on_rename_failure() {
    let data_root = tempfile::tempdir().unwrap();
    let attachment = normalize_attachment_config(
        WorkspaceId::new(),
        AttachmentConfig {
            kind: WorkspaceAttachmentKind::ReferenceRepo,
            name: "Docs".to_string(),
            source: "https://example.com/repo.git".to_string(),
            revision: Some("main".to_string()),
            subpath: None,
            mount_relpath: None,
            mode: None,
            update_policy: None,
        },
        None,
    )
    .unwrap();
    let dest = materialized_path_for_attachment(data_root.path(), &attachment);
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("existing.txt"), "existing\n").unwrap();
    let missing_temp = unique_materialized_temp_path(&dest).unwrap();

    let err = install_materialized_temp(data_root.path(), &missing_temp, &dest)
        .await
        .expect_err("missing staged temp should fail install");

    assert!(format!("{err:#}").contains("installing attachment materialization"));
    assert_eq!(
        std::fs::read_to_string(dest.join("existing.txt")).unwrap(),
        "existing\n",
        "failed install must restore the previous materialization"
    );
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        !leftovers
            .iter()
            .any(|name| name.contains("materialize-old") || name.contains("materialize-tmp")),
        "failed install left temp or backup entries: {leftovers:?}"
    );
}

fn create_reference_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let source_parent = tempfile::tempdir().unwrap();
    let source = source_parent.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();

    let init = std::process::Command::new("git")
        .arg("init")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    std::fs::write(source.join("README.md"), "docs\n").unwrap();
    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(&source)
        .arg("add")
        .arg("README.md")
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "git add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(&source)
        .arg("-c")
        .arg("user.email=test@example.com")
        .arg("-c")
        .arg("user.name=Test User")
        .arg("commit")
        .arg("-m")
        .arg("init")
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    (source_parent, source)
}

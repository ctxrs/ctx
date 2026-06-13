#[cfg(unix)]
mod common;

#[cfg(unix)]
mod unix_smoke {
    use super::common;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;
    use tokio::process::Command;

    use ctx_core::models::WorkspaceAttachmentKind;
    use ctx_fs::git::rev_parse_head;
    use ctx_workspace_attachments::AttachmentConfig;

    async fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn copy_dir_recursive(src: &Path, dest: &Path) {
        std::fs::create_dir_all(dest).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let ty = entry.file_type().unwrap();
            let target = dest.join(entry.file_name());
            if ty.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if ty.is_file() {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn fixture_root() -> PathBuf {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("core/fixtures/workspace-attachments-demo");
        repo_root
    }

    fn write_fake_docs_mirror_bin(bin_dir: &Path) -> PathBuf {
        let bin = bin_dir.join("ctx-docs-mirror");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
set -eu
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--out" ]; then
    shift
    out="$1"
  fi
  shift
done
if [ -z "$out" ]; then
  echo "missing --out" >&2
  exit 2
fi
mkdir -p "$out"
printf '<!doctype html>\n<title>React docs</title>\n' > "$out/index.html"
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bin, permissions).unwrap();
        bin
    }

    #[tokio::test]
    #[ignore]
    async fn attachments_demo_react_smoketest() {
        let temp = TempDir::new().unwrap();
        let ws_root = temp.path().join("workspace");
        copy_dir_recursive(&fixture_root(), &ws_root);

        run_git(&ws_root, &["init"]).await;
        run_git(&ws_root, &["config", "user.email", "test@example.com"]).await;
        run_git(&ws_root, &["config", "user.name", "Test"]).await;
        std::fs::write(ws_root.join("README.md"), "demo\n").unwrap();
        run_git(&ws_root, &["add", "."]).await;
        run_git(&ws_root, &["commit", "-m", "init"]).await;

        let data_dir = TempDir::new().unwrap();
        let bin_dir = TempDir::new().unwrap();
        let docs_mirror_bin = write_fake_docs_mirror_bin(bin_dir.path());
        let _env_lock = common::process_env_test_lock().lock().await;
        let _docs_mirror_bin =
            common::TestEnvGuard::set("CTX_DOCS_MIRROR_BIN", docs_mirror_bin.as_os_str());
        let base_commit_sha = rev_parse_head(&ws_root).await.unwrap();
        let fixture =
            common::fake_daemon_fixture_for_data_root(data_dir.path(), "http://127.0.0.1:0").await;
        let seeded = fixture
            .daemon
            .seed_workspace_attachments_demo_fixture_for_test("demo", &ws_root, base_commit_sha)
            .await
            .unwrap();
        assert_eq!(seeded.task.primary_worktree_id, Some(seeded.worktree.id));

        let mounts = fixture
            .daemon
            .materialize_workspace_attachments_for_test(
                &seeded.workspace,
                &seeded.worktree,
                [
                    AttachmentConfig {
                        kind: WorkspaceAttachmentKind::ReferenceRepo,
                        name: "react".to_string(),
                        source: ws_root.to_string_lossy().to_string(),
                        revision: Some("main".to_string()),
                        subpath: None,
                        mount_relpath: None,
                        mode: None,
                        update_policy: None,
                    },
                    AttachmentConfig {
                        kind: WorkspaceAttachmentKind::DocMirror,
                        name: "react-docs".to_string(),
                        source: "https://react.dev/reference/react".to_string(),
                        revision: None,
                        subpath: None,
                        mount_relpath: None,
                        mode: None,
                        update_policy: None,
                    },
                ],
            )
            .await
            .unwrap();
        assert!(!mounts.is_empty());
        assert!(ws_root.join(".ctx/attachments/refs/react").exists());
        assert!(ws_root.join(".ctx/attachments/docs/react-docs").exists());
    }
}

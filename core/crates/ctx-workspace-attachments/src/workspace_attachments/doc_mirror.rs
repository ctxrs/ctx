use super::*;
use anyhow::Context;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use tokio::process::Command;
use toml::Value as TomlValue;

pub(super) async fn materialize_doc_mirror(
    data_root: &Path,
    workspace: &Workspace,
    attachment: &WorkspaceAttachment,
    refresh: bool,
) -> Result<MaterializationResult> {
    let revision = revision_key(attachment);
    let dest = materialized_path_for_attachment(data_root, attachment);
    let should_update = refresh || !dest.exists();
    if should_update {
        let temp = super::materialized_install::unique_materialized_temp_path(&dest)?;
        super::materialized_paths::ensure_materialized_revision_parent(data_root, attachment)
            .await?;
        tokio::fs::create_dir(&temp).await?;
        if let Err(err) = run_doc_mirror_cli(workspace, attachment, &temp).await {
            super::materialized_install::cleanup_materialized_temp(data_root, &temp).await;
            return Err(err);
        }
        super::materialized_install::install_materialized_temp(data_root, &temp, &dest).await?;
    } else {
        super::validate_materialized_path(data_root, attachment).await?;
    }
    Ok(MaterializationResult {
        path: dest,
        materialized_id: revision,
    })
}

pub(super) fn validate_doc_mirror_source(
    _workspace: &Workspace,
    attachment: &WorkspaceAttachment,
) -> Result<()> {
    validate_doc_mirror_source_value(&attachment.source)
}

pub(super) fn validate_doc_mirror_source_value(source: &str) -> Result<()> {
    if looks_like_url(source) {
        return Ok(());
    }
    anyhow::bail!(
        "doc_mirror source must be an http(s) URL; executable local doc mirror scripts are not supported"
    )
}

fn docs_mirror_bin() -> PathBuf {
    std::env::var_os("CTX_DOCS_MIRROR_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ctx-docs-mirror"))
}

fn looks_like_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

async fn run_doc_mirror_cli(
    workspace: &Workspace,
    attachment: &WorkspaceAttachment,
    dest: &Path,
) -> Result<()> {
    let mut table = toml::value::Table::new();
    table.insert(
        "source".to_string(),
        TomlValue::String(attachment.source.clone()),
    );
    table.insert(
        "docs_url".to_string(),
        TomlValue::String(attachment.source.clone()),
    );
    let cfg = TomlValue::Table(table);
    let cfg_text = toml::to_string_pretty(&cfg).context("serializing docs mirror config")?;
    let mut temp = NamedTempFile::new().context("creating docs mirror config file")?;
    temp.write_all(cfg_text.as_bytes())
        .context("writing docs mirror config")?;
    temp.flush().context("flushing docs mirror config")?;

    let bin = docs_mirror_bin();
    let mut cmd = Command::new(&bin);
    cmd.arg("mirror")
        .arg("--config")
        .arg(temp.path())
        .arg("--out")
        .arg(dest)
        .current_dir(&workspace.root_path)
        .env("CTX_DOCS_OUTPUT_DIR", dest)
        .kill_on_drop(true);
    let output = cmd.output().await.context("running ctx-docs-mirror")?;
    if !output.status.success() {
        anyhow::bail!(
            "ctx-docs-mirror failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_core::ids::WorkspaceId;

    #[cfg(unix)]
    #[tokio::test]
    async fn doc_mirror_materialization_cleans_temp_and_final_on_cli_failure() {
        use std::os::unix::fs::PermissionsExt;

        let data_root = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let bin = bin_dir.path().join("ctx-docs-mirror-fail");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--out" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$out"
printf partial > "$out/partial.txt"
exit 7
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bin, permissions).unwrap();

        let workspace = Workspace {
            id: WorkspaceId::new(),
            name: "workspace".to_string(),
            root_path: workspace_root.path().to_string_lossy().to_string(),
            created_at: Utc::now(),
            vcs_kind: None,
        };
        let attachment = normalize_attachment_config(
            workspace.id,
            AttachmentConfig {
                kind: WorkspaceAttachmentKind::DocMirror,
                name: "Docs".to_string(),
                source: "https://example.com/docs".to_string(),
                revision: Some("main".to_string()),
                subpath: None,
                mount_relpath: None,
                mode: None,
                update_policy: None,
            },
            None,
        )
        .unwrap();

        std::env::set_var("CTX_DOCS_MIRROR_BIN", &bin);
        let result = materialize_doc_mirror(data_root.path(), &workspace, &attachment, true).await;
        let err = result.expect_err("failing docs mirror CLI should fail materialization");

        assert!(format!("{err:#}").contains("ctx-docs-mirror failed"));
        let dest = materialized_path_for_attachment(data_root.path(), &attachment);
        assert!(
            !dest.exists(),
            "failed doc mirror materialization must not leave final revision path"
        );
        let leftovers = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(
            !leftovers
                .iter()
                .any(|name| name.contains("materialize-tmp")),
            "failed doc mirror materialization left temp entries: {leftovers:?}"
        );

        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("existing.txt"), "existing\n").unwrap();
        let refresh_result =
            materialize_doc_mirror(data_root.path(), &workspace, &attachment, true).await;
        std::env::remove_var("CTX_DOCS_MIRROR_BIN");
        let refresh_err = refresh_result.expect_err("failing docs mirror CLI should fail refresh");

        assert!(format!("{refresh_err:#}").contains("ctx-docs-mirror failed"));
        assert_eq!(
            std::fs::read_to_string(dest.join("existing.txt")).unwrap(),
            "existing\n",
            "failed doc mirror refresh must preserve the previous materialization"
        );
        let refresh_leftovers = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(
            !refresh_leftovers
                .iter()
                .any(|name| name.contains("materialize-tmp")),
            "failed doc mirror refresh left temp entries: {refresh_leftovers:?}"
        );
    }
}

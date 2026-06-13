use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn update_registry_last_error(
    data_root: &Path,
    provider_id: &str,
    stage: &str,
    err: &anyhow::Error,
    code: InstallErrorCode,
    package: Option<&str>,
    version: Option<&str>,
    install_dir_rel: Option<String>,
    target: Option<InstallTarget>,
) {
    let install_dir_rel_clone = install_dir_rel.clone();
    let _ = mutate_agent_server_config(data_root, move |cfg| {
        let target = target.unwrap_or(InstallTarget::Host);
        let target_key = target.as_str().to_string();
        let install_targets = cfg
            .managed_install_targets
            .entry(provider_id.to_string())
            .or_default();
        let mut meta =
            install_targets
                .get(&target_key)
                .cloned()
                .unwrap_or(ManagedInstallMetadata {
                    package: package.map(|s| s.to_string()),
                    version: version.map(|s| s.to_string()),
                    artifact_fingerprint: None,
                    archive_sha256: None,
                    target: Some(target),
                    install_dir_rel: install_dir_rel_clone,
                    bin_dir_rel: None,
                    last_success_at: None,
                    last_error: None,
                });
        if meta.package.is_none() {
            meta.package = package.map(|s| s.to_string());
        }
        if meta.version.is_none() {
            meta.version = version.map(|s| s.to_string());
        }
        if meta.install_dir_rel.is_none() {
            meta.install_dir_rel = install_dir_rel;
        }
        meta.target = Some(target);

        meta.last_error = Some(ManagedInstallError {
            at: Utc::now().to_rfc3339(),
            stage: stage.to_string(),
            message: truncate_for_storage(&format!("{err:#}"), LAST_ERROR_MAX_LEN),
            code: Some(code),
        });
        install_targets.insert(target_key.clone(), meta.clone());

        if let Some(entry) = cfg
            .managed_provider_targets
            .get_mut(provider_id)
            .and_then(|targets| targets.get_mut(&target_key))
        {
            entry.managed = Some(meta);
        }
    })
    .await;
}

pub(crate) async fn repair_install_dir(
    install_id: Option<InstallId>,
    state: &ManagedInstallHostObject,
    provider_id: &str,
    install_dir: &Path,
    expected_entrypoint_rel: &str,
) -> Result<()> {
    emit_install(
        state,
        install_id,
        provider_id,
        InstallEventLevel::Info,
        "prepare",
        format!("Preparing install dir: {}", install_dir.display()),
        None,
        None,
        None,
    )
    .await;

    // Repair semantics: remove obviously-corrupted partial installs.
    if install_dir.exists() {
        let expected = install_dir.join(expected_entrypoint_rel);
        let node_modules = install_dir.join("node_modules");
        if !node_modules.exists()
            || !expected.exists()
            || gemini_acp_bundle_needs_repair(provider_id, &expected)?
        {
            tokio::fs::remove_dir_all(install_dir).await.ok();
        }
    }
    tokio::fs::create_dir_all(install_dir)
        .await
        .with_context(|| format!("creating install dir: {}", install_dir.display()))?;
    Ok(())
}

fn gemini_acp_bundle_needs_repair(provider_id: &str, expected_entrypoint: &Path) -> Result<bool> {
    if provider_id != "gemini" {
        return Ok(false);
    }
    let Some(bundle_dir) = expected_entrypoint.parent() else {
        return Ok(true);
    };
    let entries = match std::fs::read_dir(bundle_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading Gemini ACP bundle {}", bundle_dir.display()));
        }
    };
    let mut core_entry_count = 0usize;
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "reading Gemini ACP bundle entry under {}",
                bundle_dir.display()
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("js")
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    value.starts_with("core-") || value.eq_ignore_ascii_case("core.js")
                })
        {
            core_entry_count += 1;
        }
    }
    Ok(core_entry_count == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn update_registry_last_error_persists_provider_runtime_metadata_to_target_bucket_only() {
        let data_root = tempdir().expect("tempdir");
        update_registry_last_error(
            data_root.path(),
            "droid",
            "prepare",
            &anyhow::anyhow!("boom"),
            InstallErrorCode::CommandFailed,
            Some("droid"),
            Some("0.1.1"),
            Some("providers/agent-servers/droid/0.1.1".to_string()),
            Some(InstallTarget::Host),
        )
        .await;

        let cfg = load_agent_server_config(data_root.path())
            .await
            .expect("load config");
        let meta = cfg
            .managed_install_targets
            .get("droid")
            .and_then(|targets| targets.get("host"))
            .expect("droid host metadata");
        assert_eq!(meta.target, Some(InstallTarget::Host));
        assert_eq!(meta.package.as_deref(), Some("droid"));
        assert_eq!(meta.version.as_deref(), Some("0.1.1"));
        assert!(meta.last_error.is_some());
        assert!(!cfg.managed_installs.contains_key("droid"));
        assert!(
            !cfg.providers.contains_key("droid"),
            "provider install errors must not recreate legacy shared provider entries"
        );
    }

    #[test]
    fn gemini_acp_bundle_repair_accepts_duplicate_core_entries() {
        let temp = tempdir().expect("tempdir");
        let bundle_dir = temp
            .path()
            .join("node_modules")
            .join("@google")
            .join("gemini-cli")
            .join("bundle");
        std::fs::create_dir_all(&bundle_dir).expect("create bundle dir");
        let entrypoint = bundle_dir.join("gemini.js");
        std::fs::write(&entrypoint, b"gemini").expect("write entrypoint");
        std::fs::write(bundle_dir.join("core-alpha.js"), b"core").expect("write core alpha");
        std::fs::write(bundle_dir.join("core-beta.js"), b"core").expect("write core beta");

        assert!(
            !gemini_acp_bundle_needs_repair("gemini", &entrypoint)
                .expect("repair check should succeed"),
            "duplicate Gemini core entries are valid in current Gemini CLI bundles"
        );
    }

    #[test]
    fn gemini_acp_bundle_repair_accepts_single_core_entry() {
        let temp = tempdir().expect("tempdir");
        let bundle_dir = temp
            .path()
            .join("node_modules")
            .join("@google")
            .join("gemini-cli")
            .join("bundle");
        std::fs::create_dir_all(&bundle_dir).expect("create bundle dir");
        let entrypoint = bundle_dir.join("gemini.js");
        std::fs::write(&entrypoint, b"gemini").expect("write entrypoint");
        std::fs::write(bundle_dir.join("core-alpha.js"), b"core").expect("write core alpha");

        assert!(
            !gemini_acp_bundle_needs_repair("gemini", &entrypoint)
                .expect("repair check should succeed"),
            "single Gemini core entry should not force reinstall"
        );
    }
}

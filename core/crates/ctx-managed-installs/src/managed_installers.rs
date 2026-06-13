use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn install_managed_npm_provider(
    state: &ManagedInstallHostObject,
    install_id: Option<InstallId>,
    provider_id: &str,
    package: &str,
    version: &str,
    script_rel: &str,
    extra_args: Vec<String>,
    target: InstallTarget,
    stage: &mut &'static str,
) -> Result<ManagedProviderInstall> {
    let data_root = state.data_root().to_path_buf();
    let install_dir = install_dir_for_provider(&data_root, provider_id, version, target);
    let install_dir_rel = install_dir_rel(&data_root, &install_dir);

    *stage = "node";
    let node = ensure_node_runtime(state, install_id, provider_id, &data_root, target)
        .await
        .context("ensuring managed Node runtime")?;

    *stage = "prepare";
    repair_install_dir(install_id, state, provider_id, &install_dir, script_rel)
        .await
        .context("preparing install directory")?;

    let package_spec = format!("{package}@{version}");
    *stage = "npm_install";
    npm_install(
        state,
        install_id,
        provider_id,
        &node,
        &install_dir,
        &package_spec,
        target,
    )
    .await
    .context("running package install")?;

    *stage = "entrypoint";
    let script_path = install_dir.join(script_rel);
    if !script_path.exists() {
        tokio::fs::remove_dir_all(&install_dir).await.ok();
        anyhow::bail!(
            "install completed but entrypoint missing: {}",
            script_path.display()
        );
    }

    let mut args = vec![script_path.to_string_lossy().to_string()];
    args.extend(extra_args);

    let meta = ManagedInstallMetadata {
        package: Some(package.to_string()),
        version: Some(version.to_string()),
        artifact_fingerprint: npm_artifact_fingerprint(package, version),
        archive_sha256: None,
        target: Some(target),
        install_dir_rel: Some(install_dir_rel),
        bin_dir_rel: None,
        last_success_at: Some(Utc::now().to_rfc3339()),
        last_error: None,
    };

    Ok(ManagedProviderInstall {
        command: node.node_bin.to_string_lossy().to_string(),
        args,
        meta,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn install_managed_archive_provider(
    state: &ManagedInstallHostObject,
    install_id: Option<InstallId>,
    provider_id: &str,
    version: &str,
    url: &str,
    expected_sha256: Option<&str>,
    archive: AgentServerArchive,
    bin_path: &str,
    args: Vec<String>,
    target: InstallTarget,
    stage: &mut &'static str,
) -> Result<ManagedProviderInstall> {
    let bin = install_agent_server_url_binary(
        state,
        install_id,
        provider_id,
        provider_id,
        version,
        url,
        expected_sha256,
        archive,
        bin_path,
        target,
        stage,
    )
    .await
    .context("installing agent server binary")?;

    let install_dir = install_dir_for_provider(state.data_root(), provider_id, version, target);
    let meta = ManagedInstallMetadata {
        package: Some(url.to_string()),
        version: Some(version.to_string()),
        artifact_fingerprint: expected_sha256.map(str::to_string),
        archive_sha256: expected_sha256.map(str::to_string),
        target: Some(target),
        install_dir_rel: Some(install_dir_rel(state.data_root(), &install_dir)),
        bin_dir_rel: None,
        last_success_at: Some(Utc::now().to_rfc3339()),
        last_error: None,
    };

    Ok(ManagedProviderInstall {
        command: bin.to_string_lossy().to_string(),
        args,
        meta,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn install_managed_python_provider(
    state: &ManagedInstallHostObject,
    install_id: Option<InstallId>,
    provider_id: &str,
    package: &str,
    version: &str,
    entrypoint: &str,
    python_version: Option<&str>,
    python_build_tag: Option<&str>,
    args: Vec<String>,
    target: InstallTarget,
    stage: &mut &'static str,
) -> Result<ManagedProviderInstall> {
    *stage = "python";
    let python_runtime = managed_python_runtime_spec(python_version, python_build_tag);
    let python_runtime_install = ensure_python_runtime_versioned(
        state,
        install_id,
        provider_id,
        state.data_root(),
        target,
        &python_runtime.version,
        &python_runtime.build_tag,
    )
    .await
    .context("ensuring managed Python runtime")?;
    let python = python_runtime_install.python_bin.clone();
    let data_root = state.data_root().to_path_buf();
    let install_dir = install_dir_for_provider(&data_root, provider_id, version, target);
    let install_dir_rel = install_dir_rel(&data_root, &install_dir);
    let venv_dir = install_dir.join("venv");

    *stage = "prepare";
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

    if install_dir.exists() {
        let expected = venv_exe(&venv_dir, entrypoint, target);
        if !expected.exists() {
            tokio::fs::remove_dir_all(&install_dir).await.ok();
        }
    }
    tokio::fs::create_dir_all(&install_dir)
        .await
        .with_context(|| format!("creating install dir: {}", install_dir.display()))?;

    *stage = "venv";
    emit_install(
        state,
        install_id,
        provider_id,
        InstallEventLevel::Info,
        "venv",
        "Creating virtualenv…".to_string(),
        None,
        None,
        None,
    )
    .await;

    if matches!(target, InstallTarget::Container) {
        state
            .ensure_builder_ready()
            .await
            .context("ensuring container builder readiness")?;
        let argv = vec![
            python.to_string_lossy().to_string(),
            "-m".to_string(),
            "venv".to_string(),
            venv_dir.to_string_lossy().to_string(),
        ];
        let out = state
            .run_builder_command(&install_dir, &[], &argv, Duration::from_secs(5 * 60))
            .await
            .context("creating virtualenv")?;
        if !out.status.success() {
            anyhow::bail!(
                "creating virtualenv failed status={}\nstdout:\n{}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    } else {
        let mut venv_cmd = Command::new(&python);
        venv_cmd
            .arg("-m")
            .arg("venv")
            .arg(&venv_dir)
            .kill_on_drop(true);
        run_command_with_timeout(venv_cmd, Duration::from_secs(5 * 60))
            .await
            .context("creating virtualenv")?;
    }

    let venv_python = venv_exe(&venv_dir, "python", target);

    if matches!(target, InstallTarget::Container) {
        let argv = vec![
            venv_python.to_string_lossy().to_string(),
            "-m".to_string(),
            "ensurepip".to_string(),
            "--upgrade".to_string(),
        ];
        let out = state
            .run_builder_command(&install_dir, &[], &argv, Duration::from_secs(5 * 60))
            .await
            .context("ensuring pip in virtualenv")?;
        if !out.status.success() {
            anyhow::bail!(
                "ensurepip failed status={}\nstdout:\n{}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    } else {
        ensure_python_pip(&venv_python)
            .await
            .context("ensuring pip in virtualenv")?;
    }

    let package_spec = if package.starts_with("https://") || package.starts_with("http://") {
        package.to_string()
    } else {
        format!("{package}=={version}")
    };
    let pip_policy = crate::install_policy::validate_pip_install_policy(&package_spec)?;

    *stage = "pip_install";
    emit_install(
        state,
        install_id,
        provider_id,
        InstallEventLevel::Info,
        "pip_install",
        format!("Installing {package_spec}…"),
        None,
        None,
        None,
    )
    .await;

    let out = if matches!(target, InstallTarget::Container) {
        let argv = vec![
            venv_python.to_string_lossy().to_string(),
            "-m".to_string(),
            "pip".to_string(),
            "install".to_string(),
            "--disable-pip-version-check".to_string(),
            "--no-input".to_string(),
            package_spec.clone(),
        ];
        let mut env = vec![("PIP_DISABLE_PIP_VERSION_CHECK".to_string(), "1".to_string())];
        if let Some(index_url) = pip_policy.index_url.as_ref() {
            env.push(("PIP_INDEX_URL".to_string(), index_url.clone()));
        }
        state
            .run_builder_command(&install_dir, &env, &argv, PIP_INSTALL_TIMEOUT)
            .await
    } else {
        let mut pip_cmd = Command::new(&venv_python);
        pip_cmd
            .arg("-m")
            .arg("pip")
            .arg("install")
            .arg("--disable-pip-version-check")
            .arg("--no-input")
            .arg(&package_spec)
            .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
            .kill_on_drop(true);
        if let Some(index_url) = pip_policy.index_url.as_ref() {
            pip_cmd.env("PIP_INDEX_URL", index_url);
        }
        run_command_with_timeout(pip_cmd, PIP_INSTALL_TIMEOUT).await
    }
    .context("running pip install")?;
    if !out.status.success() {
        anyhow::bail!(
            "pip install failed ({}) status={}\nstdout:\n{}\nstderr:\n{}",
            package_spec,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let exe = venv_exe(&venv_dir, entrypoint, target);
    if !exe.exists() {
        tokio::fs::remove_dir_all(&install_dir).await.ok();
        anyhow::bail!(
            "pip install completed but entrypoint missing: {}",
            exe.display()
        );
    }

    let meta = ManagedInstallMetadata {
        package: Some(package.to_string()),
        version: Some(version.to_string()),
        artifact_fingerprint: python_artifact_fingerprint(
            package,
            version,
            Some(&python_runtime.version),
            Some(&python_runtime.build_tag),
            python_runtime_install.archive_sha256.as_deref(),
        ),
        archive_sha256: None,
        target: Some(target),
        install_dir_rel: Some(install_dir_rel),
        bin_dir_rel: None,
        last_success_at: Some(Utc::now().to_rfc3339()),
        last_error: None,
    };

    Ok(ManagedProviderInstall {
        command: exe.to_string_lossy().to_string(),
        args,
        meta,
    })
}

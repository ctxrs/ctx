use super::*;

pub async fn npm_install(
    state: &ManagedInstallHostObject,
    install_id: Option<InstallId>,
    provider_id: &str,
    node: &NodeRuntime,
    install_dir: &Path,
    package_spec: &str,
    target: InstallTarget,
) -> Result<()> {
    let managed_registry = crate::install_policy::validate_npm_install_policy(package_spec)?;
    let cache_dir = install_dir.join(".npm-cache");
    tokio::fs::create_dir_all(&cache_dir).await.ok();
    let node_bin_dir = node.node_bin.parent().unwrap_or(node.node_root.as_path());
    let path_sep = if cfg!(windows) { ";" } else { ":" };
    let mut combined_path = std::ffi::OsString::new();
    combined_path.push(node_bin_dir);
    combined_path.push(path_sep);
    if let Some(existing) = std::env::var_os("PATH") {
        combined_path.push(existing);
    }
    let pnpm_bin = if matches!(target, InstallTarget::Host) {
        which::which("pnpm").ok()
    } else {
        None
    };
    let package_manager = if pnpm_bin.is_some() { "pnpm" } else { "npm" };

    for attempt in 1..=RETRY_COUNT {
        ensure_install_not_cancelled(state, install_id).await?;
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Info,
            "npm_install",
            format!("{package_manager} install {package_spec} (attempt {attempt}/{RETRY_COUNT})"),
            None,
            None,
            Some(attempt),
        )
        .await;

        let out = if matches!(target, InstallTarget::Container) {
            state
                .ensure_builder_ready()
                .await
                .context("ensuring container builder readiness")?;
            let mut argv = Vec::with_capacity(16);
            argv.push(node.node_bin.to_string_lossy().to_string());
            argv.push(node.npm_cli_js.to_string_lossy().to_string());
            argv.push("install".to_string());
            argv.push("--prefix".to_string());
            argv.push(install_dir.to_string_lossy().to_string());
            argv.push("--no-audit".to_string());
            argv.push("--no-fund".to_string());
            argv.push("--silent".to_string());
            argv.push("--ignore-scripts".to_string());
            argv.push(package_spec.to_string());
            let mut env = vec![
                (
                    "PATH".to_string(),
                    combined_path.to_string_lossy().to_string(),
                ),
                (
                    "npm_config_update_notifier".to_string(),
                    "false".to_string(),
                ),
                ("npm_config_fund".to_string(), "false".to_string()),
                ("npm_config_audit".to_string(), "false".to_string()),
                ("npm_config_progress".to_string(), "false".to_string()),
                (
                    "npm_config_cache".to_string(),
                    cache_dir.to_string_lossy().to_string(),
                ),
                ("npm_config_ignore_scripts".to_string(), "true".to_string()),
            ];
            if let Some(registry) = managed_registry.as_ref() {
                env.push(("npm_config_registry".to_string(), registry.clone()));
            }
            state
                .run_builder_command(install_dir, &env, &argv, NPM_INSTALL_TIMEOUT)
                .await
        } else {
            let mut cmd = if let Some(pnpm) = pnpm_bin.as_ref() {
                let mut cmd = Command::new(pnpm);
                cmd.arg("add")
                    .arg("--dir")
                    .arg(install_dir)
                    .arg("--ignore-scripts")
                    .arg("--lockfile=false")
                    .arg("--reporter")
                    .arg("silent")
                    .arg(package_spec);
                if let Some(registry) = managed_registry.as_ref() {
                    cmd.arg("--registry").arg(registry);
                }
                cmd
            } else {
                let mut cmd = Command::new(&node.node_bin);
                cmd.arg(&node.npm_cli_js)
                    .arg("install")
                    .arg("--prefix")
                    .arg(install_dir)
                    .arg("--no-audit")
                    .arg("--no-fund")
                    .arg("--silent")
                    .arg("--ignore-scripts")
                    .arg(package_spec)
                    .env("npm_config_update_notifier", "false")
                    .env("npm_config_fund", "false")
                    .env("npm_config_audit", "false")
                    .env("npm_config_progress", "false")
                    .env("npm_config_cache", cache_dir.clone())
                    .env("npm_config_ignore_scripts", "true");
                cmd
            };
            if let Some(registry) = managed_registry.as_ref() {
                cmd.env("npm_config_registry", registry);
            }
            cmd.env("PATH", combined_path.clone()).kill_on_drop(true);
            run_command_with_timeout(cmd, NPM_INSTALL_TIMEOUT).await
        };
        let out = match out {
            Ok(out) => out,
            Err(e) => {
                emit_install(
                    state,
                    install_id,
                    provider_id,
                    InstallEventLevel::Error,
                    "npm_install",
                    format!("{package_manager} install failed: {e:#}"),
                    None,
                    None,
                    Some(attempt),
                )
                .await;
                if attempt < RETRY_COUNT {
                    tokio::time::sleep(Duration::from_millis(
                        RETRY_BACKOFF_BASE_MS * attempt as u64,
                    ))
                    .await;
                    continue;
                }
                return Err(e.context("running package install"));
            }
        };
        if out.status.success() {
            emit_install(
                state,
                install_id,
                provider_id,
                InstallEventLevel::Success,
                "npm_install",
                format!("{package_manager} install succeeded"),
                None,
                None,
                Some(attempt),
            )
            .await;
            return Ok(());
        }

        let err_txt = format!(
            "{package_manager} install failed ({package_spec}) status={}\nstdout:\n{}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Error,
            "npm_install",
            truncate_for_storage(&err_txt, INSTALL_EVENT_ERROR_MAX_LEN),
            None,
            None,
            Some(attempt),
        )
        .await;
        if attempt < RETRY_COUNT {
            tokio::time::sleep(Duration::from_millis(
                RETRY_BACKOFF_BASE_MS * attempt as u64,
            ))
            .await;
        }
    }

    anyhow::bail!(
        "{package_manager} install failed after {RETRY_COUNT} attempts ({package_spec}). Try again, or check your network / npm registry access."
    );
}

pub fn sanitize_npm_package_for_path(pkg: &str) -> String {
    pkg.trim()
        .trim_start_matches('@')
        .replace(['/', '\\'], "__")
}

pub async fn npm_install_one(
    state: &ManagedInstallHostObject,
    install_id: Option<InstallId>,
    provider_id: &str,
    node: &NodeRuntime,
    install_dir: &Path,
    package: &str,
    version: &str,
) -> Result<()> {
    let package_spec = format!("{package}@{version}");
    npm_install(
        state,
        install_id,
        provider_id,
        node,
        install_dir,
        &package_spec,
        InstallTarget::Host,
    )
    .await
}

pub async fn npm_dependency_matches(
    install_dir: &Path,
    package: &str,
    version: &str,
) -> Result<bool> {
    let pkg_dir = install_dir.join("node_modules").join(package);
    let pkg_json_path = pkg_dir.join("package.json");
    if !pkg_json_path.exists() {
        return Ok(false);
    }
    let txt = tokio::fs::read_to_string(&pkg_json_path)
        .await
        .with_context(|| format!("reading {}", pkg_json_path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&txt).context("parsing package.json")?;
    Ok(v.get("version")
        .and_then(|v| v.as_str())
        .map(|v| v == version)
        .unwrap_or(false))
}

pub async fn resolve_node_package_bin(
    install_dir: &Path,
    package: &str,
    preferred_bin_name: Option<&str>,
) -> Result<PathBuf> {
    let pkg_dir = install_dir.join("node_modules").join(package);
    let pkg_json_path = pkg_dir.join("package.json");
    let txt = tokio::fs::read_to_string(&pkg_json_path)
        .await
        .with_context(|| format!("reading {}", pkg_json_path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&txt).context("parsing package.json")?;
    let bin = v.get("bin").context("package.json missing bin")?;
    let entry_rel = match bin {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => {
            if let Some(preferred) = preferred_bin_name {
                if let Some(v) = map.get(preferred).and_then(|v| v.as_str()) {
                    v.to_string()
                } else if map.len() == 1 {
                    map.values()
                        .next()
                        .and_then(|v| v.as_str())
                        .context("bin map invalid")?
                        .to_string()
                } else {
                    anyhow::bail!(
                        "bin {preferred} not found in {} (available: {})",
                        package,
                        map.keys().cloned().collect::<Vec<_>>().join(", ")
                    );
                }
            } else if map.len() == 1 {
                map.values()
                    .next()
                    .and_then(|v| v.as_str())
                    .context("bin map invalid")?
                    .to_string()
            } else {
                anyhow::bail!("multiple bins in {package} but no preferred bin specified");
            }
        }
        _ => anyhow::bail!("package.json bin has unsupported type"),
    };
    Ok(pkg_dir.join(entry_rel))
}

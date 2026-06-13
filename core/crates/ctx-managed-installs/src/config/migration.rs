use super::*;

fn env_flag_truthy(var_name: &str) -> bool {
    match std::env::var(var_name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

pub(super) fn bundled_only_mode_applies_to_provider(provider_id: &str) -> bool {
    if !env_flag_truthy("CTX_E2E_BUNDLED_ONLY") {
        return false;
    }
    let raw = match std::env::var("CTX_E2E_BUNDLED_ONLY_PROVIDERS") {
        Ok(value) => value,
        Err(_) => return true,
    };
    let providers: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    if providers.is_empty() {
        return true;
    }
    providers.contains(&provider_id)
}

fn is_legacy_bundle_path(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/contents/resources/bundles/")
        || normalized.contains("/src-tauri/bundles/")
        || normalized.contains("/bundles/providers/")
}

fn is_legacy_bundle_rel(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/").to_ascii_lowercase();
    normalized.starts_with("bundles/")
        || normalized.starts_with("./bundles/")
        || normalized.contains("/bundles/")
}

fn migrate_codex_adapter_key_to_provider_key<T>(map: &mut HashMap<String, T>) -> bool {
    let Some(adapter_value) = map.remove(ctx_core::provider_ids::CODEX_CRP_ADAPTER_ID) else {
        return false;
    };
    map.entry(ctx_core::provider_ids::CODEX_PROVIDER_ID.to_string())
        .or_insert(adapter_value);
    true
}

pub(super) fn migrate_agent_server_config(cfg: &mut AgentServerConfigFile) -> bool {
    let mut changed = false;
    let mut drop_provider_entries = Vec::new();
    let mut drop_managed_entries = Vec::new();

    changed |= migrate_codex_adapter_key_to_provider_key(&mut cfg.providers);
    changed |= migrate_codex_adapter_key_to_provider_key(&mut cfg.provider_login_executables);
    changed |= migrate_codex_adapter_key_to_provider_key(&mut cfg.provider_login_commands);
    changed |= migrate_codex_adapter_key_to_provider_key(&mut cfg.managed_installs);
    changed |= migrate_codex_adapter_key_to_provider_key(&mut cfg.managed_provider_targets);
    changed |= migrate_codex_adapter_key_to_provider_key(&mut cfg.managed_install_targets);

    if !cfg.provider_login_commands.is_empty() {
        for (provider_id, legacy) in std::mem::take(&mut cfg.provider_login_commands) {
            if provider_id == "claude-cli" {
                cfg.providers.entry(provider_id).or_insert(legacy);
                continue;
            }
            cfg.provider_login_executables
                .entry(provider_id)
                .or_insert_with(|| ProviderLoginExecutable {
                    executable_path: legacy.command,
                });
        }
        changed = true;
    }

    for (provider_id, command) in cfg.providers.iter_mut() {
        if migrate_managed_provider_command_args(provider_id, command) {
            changed = true;
        }
        let Some(existing) = command.managed.as_ref() else {
            continue;
        };
        let target = infer_legacy_managed_target(provider_id, existing.target);
        if command.managed.as_ref().and_then(|managed| managed.target) != Some(target) {
            if let Some(managed) = command.managed.as_mut() {
                managed.target = Some(target);
            }
            changed = true;
        }
        let Some(managed) = command.managed.clone() else {
            continue;
        };
        let has_legacy_rel = managed
            .install_dir_rel
            .as_deref()
            .map(is_legacy_bundle_rel)
            .unwrap_or(false)
            || managed
                .bin_dir_rel
                .as_deref()
                .map(is_legacy_bundle_rel)
                .unwrap_or(false);

        if is_legacy_bundle_path(&command.command) || has_legacy_rel {
            drop_provider_entries.push(provider_id.clone());
            drop_managed_entries.push(provider_id.clone());
            continue;
        }
        let mut command_clone = command.clone();
        command_clone.managed = Some(managed.clone());

        let target_key = install_target_bucket_key(target);
        let provider_targets = cfg
            .managed_provider_targets
            .entry(provider_id.clone())
            .or_default();
        if !provider_targets.contains_key(target_key) {
            provider_targets.insert(target_key.to_string(), command_clone);
        }
        let install_targets = cfg
            .managed_install_targets
            .entry(provider_id.clone())
            .or_default();
        if !install_targets.contains_key(target_key) {
            install_targets.insert(target_key.to_string(), managed.clone());
        }
        drop_provider_entries.push(provider_id.clone());
        changed = true;
    }

    for (provider_id, targets) in cfg.managed_provider_targets.iter_mut() {
        for command in targets.values_mut() {
            if migrate_managed_provider_command_args(provider_id, command) {
                changed = true;
            }
        }
    }

    for (provider_id, managed) in cfg.managed_installs.iter_mut() {
        let target = infer_legacy_managed_target(provider_id, managed.target);
        if managed.target != Some(target) {
            managed.target = Some(target);
            changed = true;
        }
        if expected_managed_dependency_version(provider_id).is_some() {
            continue;
        }
        let has_legacy_rel = managed
            .install_dir_rel
            .as_deref()
            .map(is_legacy_bundle_rel)
            .unwrap_or(false)
            || managed
                .bin_dir_rel
                .as_deref()
                .map(is_legacy_bundle_rel)
                .unwrap_or(false);
        if has_legacy_rel {
            drop_managed_entries.push(provider_id.clone());
            continue;
        }

        let target_key = install_target_bucket_key(target);
        let install_targets = cfg
            .managed_install_targets
            .entry(provider_id.clone())
            .or_default();
        if !install_targets.contains_key(target_key) {
            install_targets.insert(target_key.to_string(), managed.clone());
        }
        drop_managed_entries.push(provider_id.clone());
        changed = true;
    }

    if !drop_provider_entries.is_empty() {
        for provider_id in drop_provider_entries {
            cfg.providers.remove(&provider_id);
        }
        changed = true;
    }
    if !drop_managed_entries.is_empty() {
        drop_managed_entries.sort();
        drop_managed_entries.dedup();
        for provider_id in drop_managed_entries {
            cfg.managed_installs.remove(&provider_id);
        }
        changed = true;
    }

    changed
}

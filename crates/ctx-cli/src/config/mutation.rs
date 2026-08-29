use super::*;

const RETIRED_UPGRADE_FAKE_IP_CONTROL: &str = "upgrade.allow_rfc2544_fake_ip";

// TODO(config-cleanup): Remove this compatibility migration after 2026-11-30
// and after at least three stable releases following 1.2.1 have been available.
pub(super) fn read_config_text_migrating_retired_controls(path: &Path) -> Result<Option<String>> {
    let text = read_optional_config_text(path)?;
    let Some(text) = text else {
        return Ok(None);
    };
    let Some(updated) = remove_retired_upgrade_fake_ip_key(path, &text)? else {
        return Ok(Some(text));
    };

    // The migration is best-effort compatibility work, not a new reason to
    // reject a previously readable config. If the lock or durable rewrite is
    // unavailable, use the validated migrated bytes in memory and retry on a
    // later load.
    let Ok(_mutation_lock) = ConfigMutationLock::acquire(path) else {
        return Ok(Some(updated));
    };
    read_config_text_migrating_retired_controls_lock_held(path)
}

fn read_config_text_migrating_retired_controls_lock_held(path: &Path) -> Result<Option<String>> {
    let text = read_optional_config_text(path)?;
    let Some(text) = text else {
        return Ok(None);
    };
    let Some(updated) = remove_retired_upgrade_fake_ip_key(path, &text)? else {
        return Ok(Some(text));
    };
    let _ = write_config_durably(path, updated.as_bytes());
    Ok(Some(updated))
}

fn remove_retired_upgrade_fake_ip_key(path: &Path, text: &str) -> Result<Option<String>> {
    if !text.contains(
        RETIRED_UPGRADE_FAKE_IP_CONTROL
            .rsplit_once('.')
            .expect("retired config control must include its table")
            .1,
    ) {
        return Ok(None);
    }
    let parsed = parse_toml_subset(text).with_context(|| format!("parse {}", path.display()))?;
    let Some(retired) = parsed.get(RETIRED_UPGRADE_FAKE_IP_CONTROL) else {
        return Ok(None);
    };
    let updated = remove_config_line(text, retired.line)?;
    validated_persisted_config(path, &updated)
        .with_context(|| format!("validate migrated {}", path.display()))?;
    Ok((updated != text).then_some(updated))
}

fn remove_config_line(text: &str, line_number: usize) -> Result<String> {
    let start = if line_number == 1 {
        0
    } else {
        text.match_indices('\n')
            .nth(line_number.saturating_sub(2))
            .map(|(index, _)| index + 1)
            .ok_or_else(|| anyhow::anyhow!("retired config line is outside the parsed document"))?
    };
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset + 1);
    let mut updated = String::with_capacity(text.len().saturating_sub(end - start));
    updated.push_str(&text[..start]);
    updated.push_str(&text[end..]);
    Ok(updated)
}

fn read_optional_config_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

pub fn write_default_config(data_root: &Path) -> Result<()> {
    establish_private_data_root(data_root)?;
    Ok(())
}

pub fn set_daemon_enabled(data_root: &Path, enabled: bool) -> Result<()> {
    set_indexing_mode(data_root, IndexingMode::from_legacy_daemon_enabled(enabled))
}

pub fn persisted_daemon_enabled(data_root: &Path) -> Result<bool> {
    Ok(AppConfig::load_persisted(data_root)?
        .indexing
        .mode
        .is_automatic())
}

pub fn set_indexing_mode(data_root: &Path, mode: IndexingMode) -> Result<()> {
    establish_private_data_root(data_root)?;
    let path = AppConfig::config_path(data_root);
    let _mutation_lock = ConfigMutationLock::acquire(&path)?;
    let text = read_config_text(&path)?;
    let parsed = parse_toml_subset(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut config = AppConfig::default();
    config
        .apply_values(&parsed)
        .with_context(|| format!("load {}", path.display()))?;
    config
        .validate_provider_root_data_root(data_root)
        .with_context(|| format!("load {}", path.display()))?;

    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;
    if document.as_table().get("indexing").is_none() {
        document
            .as_table_mut()
            .insert("indexing", toml_edit::table());
    }
    let indexing = document
        .as_table_mut()
        .get_mut("indexing")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("indexing configuration must be a table"))?;
    indexing.insert("mode", toml_edit::value(mode.as_str()));
    if let Some(daemon) = document
        .as_table_mut()
        .get_mut("daemon")
        .and_then(toml_edit::Item::as_table_mut)
    {
        daemon.remove("enabled");
    }
    let updated = document.to_string();
    let parsed =
        parse_toml_subset(&updated).with_context(|| format!("parse updated {}", path.display()))?;
    let mut config = AppConfig::default();
    config
        .apply_values(&parsed)
        .with_context(|| format!("load updated {}", path.display()))?;
    config
        .validate_provider_root_data_root(data_root)
        .with_context(|| format!("load updated {}", path.display()))?;
    if updated != text {
        write_config_durably(&path, updated.as_bytes())?;
    }
    Ok(())
}

pub fn set_semantic_search_enabled(data_root: &Path, enabled: bool) -> Result<()> {
    set_config_bool(data_root, "search", "semantic", enabled)
}

pub fn set_semantic_search_enabled_with_executor(
    data_root: &Path,
    executor: &ctx_daemon_cli::SemanticEmbeddingExecutorConfig,
) -> Result<()> {
    establish_private_data_root(data_root)?;
    let path = AppConfig::config_path(data_root);
    let _mutation_lock = ConfigMutationLock::acquire(&path)?;
    let text = read_config_text(&path)?;
    validated_persisted_config(&path, &text)?;

    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;
    match (executor.http_endpoint(), executor.external_space()) {
        (Some(endpoint), Some(space)) => {
            if document.as_table().get("semantic").is_none() {
                document
                    .as_table_mut()
                    .insert("semantic", toml_edit::table());
            }
            let semantic = document
                .as_table_mut()
                .get_mut("semantic")
                .and_then(toml_edit::Item::as_table_mut)
                .ok_or_else(|| anyhow::anyhow!("semantic configuration must be a table"))?;
            semantic.insert("executor", toml_edit::value(endpoint));
            semantic.insert("space_id", toml_edit::value(space.space_id()));
            semantic.insert(
                "dimensions",
                toml_edit::value(i64::try_from(space.dimensions()).map_err(|_| {
                    anyhow::anyhow!("semantic dimensions exceed the TOML integer range")
                })?),
            );
        }
        (None, None) => {
            let remove_semantic = if let Some(semantic) = document
                .as_table_mut()
                .get_mut("semantic")
                .and_then(toml_edit::Item::as_table_mut)
            {
                semantic.remove("executor");
                semantic.remove("space_id");
                semantic.remove("dimensions");
                semantic.is_empty()
            } else {
                false
            };
            if remove_semantic {
                document.as_table_mut().remove("semantic");
            }
        }
        _ => bail!("semantic executor configuration is internally inconsistent"),
    }
    let updated = set_toml_bool(&document.to_string(), "search", "semantic", true);
    validated_persisted_config(&path, &updated)
        .with_context(|| format!("validate updated {}", path.display()))?;
    if updated != text {
        write_config_durably(&path, updated.as_bytes())?;
    }
    Ok(())
}

pub(super) fn set_config_bool(
    data_root: &Path,
    section: &str,
    key: &str,
    enabled: bool,
) -> Result<()> {
    establish_private_data_root(data_root)?;
    let path = AppConfig::config_path(data_root);
    let _mutation_lock = ConfigMutationLock::acquire(&path)?;
    let text = read_config_text(&path)?;
    let parsed = parse_toml_subset(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut config = AppConfig::default();
    config
        .apply_values(&parsed)
        .with_context(|| format!("load {}", path.display()))?;
    config
        .validate_provider_root_data_root(data_root)
        .with_context(|| format!("load {}", path.display()))?;
    let updated = set_toml_bool(&text, section, key, enabled);
    let parsed =
        parse_toml_subset(&updated).with_context(|| format!("parse updated {}", path.display()))?;
    let mut config = AppConfig::default();
    config
        .apply_values(&parsed)
        .with_context(|| format!("load updated {}", path.display()))?;
    config
        .validate_provider_root_data_root(data_root)
        .with_context(|| format!("load updated {}", path.display()))?;
    if updated == text {
        return Ok(());
    }
    write_config_durably(&path, updated.as_bytes())?;
    Ok(())
}

fn set_toml_bool(text: &str, section: &str, key: &str, enabled: bool) -> String {
    let rendered = format!("{key} = {enabled}");
    let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut current_section = String::new();
    let mut section_start = None;
    let mut insert_before = lines.len();
    for (index, raw_line) in lines.iter().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            if section_start.is_some() && current_section == section {
                insert_before = index;
                break;
            }
            current_section = line[1..line.len() - 1].trim().to_owned();
            if current_section == section {
                section_start = Some(index);
                insert_before = lines.len();
            }
            continue;
        }
        if current_section == section {
            if let Some((candidate, _)) = line.split_once('=') {
                if candidate.trim() == key {
                    lines[index] = rendered;
                    return ensure_trailing_newline(lines.join("\n"));
                }
            }
        }
    }
    match section_start {
        Some(start) => {
            let insert_at = insert_before.max(start + 1);
            lines.insert(insert_at, rendered);
        }
        None => {
            if !lines.last().is_none_or(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(rendered);
        }
    }
    ensure_trailing_newline(lines.join("\n"))
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[derive(Debug, Clone)]
pub struct ProviderRootMutation {
    pub root: ProviderRootDefinition,
    pub changed: bool,
    pub replaced: bool,
}

#[cfg(test)]
pub fn add_claude_root(
    data_root: &Path,
    id: &str,
    root: &Path,
    group: Option<&str>,
    replace: bool,
) -> Result<ProviderRootMutation> {
    add_provider_root_with_kind(
        data_root,
        id,
        CaptureProvider::Claude,
        root,
        group,
        None,
        replace,
    )
}

pub fn add_provider_root_with_kind(
    data_root: &Path,
    id: &str,
    provider: CaptureProvider,
    root: &Path,
    group: Option<&str>,
    kind: Option<ProviderRootKind>,
    replace: bool,
) -> Result<ProviderRootMutation> {
    validate_root_selector("provider root name", id)?;
    establish_private_data_root(data_root)?;
    let path = AppConfig::config_path(data_root);
    let _mutation_lock = ConfigMutationLock::acquire(&path)?;
    let text = read_config_text(&path)?;
    let current = validated_persisted_config(&path, &text)?;
    if let Some(existing) = current.provider_roots.get(id) {
        if existing.provider != provider {
            bail!(
                "provider root `{id}` is configured for {}; its provider cannot be changed to {} under the same stable name",
                existing.provider.as_str(),
                provider.as_str()
            );
        }
    }
    if let Some(group) = group {
        validate_root_selector("source group", group)?;
    }
    validate_provider_root_kind(provider, kind)?;
    let root = validated_provider_root_path(data_root, provider, root)?;
    let desired = ProviderRootDefinition {
        id: id.to_owned(),
        provider,
        path: root,
        group: group.map(str::to_owned),
        kind,
    };
    if let Some(conflicting) = current.provider_roots.values().find(|existing| {
        existing.id != id
            && existing.provider == provider
            && provider_paths_equivalent(&existing.path, &desired.path)
    }) {
        bail!(
            "{} history root `{id}` resolves to the same physical root as `{}`",
            provider.as_str(),
            conflicting.id
        );
    }
    if let Some(conflicting) = current.provider_roots.values().find(|existing| {
        existing.id != id && existing.openhands_selected_histories_overlap(&desired)
    }) {
        bail!(
            "openhands history root `{id}` overlaps legacy/current history selected by `{}`",
            conflicting.id
        );
    }
    if let Some(existing) = current.provider_roots.get(id) {
        if existing == &desired {
            return Ok(ProviderRootMutation {
                root: existing.clone(),
                changed: false,
                replaced: false,
            });
        }
        if !replace {
            bail!(
                "provider root `{id}` already exists with different settings; pass --replace to atomically replace its kind, path, and group"
            );
        }
        let mut document = text
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("parse {}", path.display()))?;
        let root = configured_root_table_mut(&mut document, id)?;
        root.insert("path", toml_edit::value(desired.path.display().to_string()));
        match desired.kind {
            Some(kind) => {
                root.insert("kind", toml_edit::value(kind.as_str()));
            }
            None => {
                root.remove("kind");
            }
        }
        match desired.group.as_deref() {
            Some(group) => {
                root.insert("group", toml_edit::value(group));
            }
            None => {
                root.remove("group");
            }
        }
        persist_validated_document(&path, document)?;
        return Ok(ProviderRootMutation {
            root: desired,
            changed: true,
            replaced: true,
        });
    }

    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;
    let roots = ensure_nested_table(&mut document, "sources", "roots")?;
    let mut item = toml_edit::Table::new();
    item.insert("provider", toml_edit::value(provider.as_str()));
    item.insert("path", toml_edit::value(desired.path.display().to_string()));
    if let Some(kind) = desired.kind {
        item.insert("kind", toml_edit::value(kind.as_str()));
    }
    if let Some(group) = desired.group.as_deref() {
        item.insert("group", toml_edit::value(group));
    }
    roots.insert(id, toml_edit::Item::Table(item));
    persist_validated_document(&path, document)?;
    Ok(ProviderRootMutation {
        root: desired,
        changed: true,
        replaced: false,
    })
}

pub fn remove_provider_root(data_root: &Path, id: &str) -> Result<ProviderRootMutation> {
    validate_root_selector("provider root name", id)?;
    establish_private_data_root(data_root)?;
    let path = AppConfig::config_path(data_root);
    let _mutation_lock = ConfigMutationLock::acquire(&path)?;
    let text = read_config_text(&path)?;
    let current = validated_persisted_config(&path, &text)?;
    let Some(existing) = current.provider_roots.get(id).cloned() else {
        bail!("provider root `{id}` is not configured");
    };
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;
    let roots = configured_roots_table_mut(&mut document)?;
    if roots.remove(id).is_none() {
        bail!("provider root `{id}` disappeared during configuration update");
    }
    persist_validated_document(&path, document)?;
    Ok(ProviderRootMutation {
        root: existing,
        changed: true,
        replaced: false,
    })
}

fn validated_provider_root_path(
    data_root: &Path,
    provider: CaptureProvider,
    root: &Path,
) -> Result<PathBuf> {
    validate_provider_root_support(provider)?;
    validate_provider_root_path(root)?;
    validate_provider_root_existing_kind(provider, root)?;
    let root = fs::canonicalize(root).with_context(|| {
        format!(
            "canonicalize {} history root {}",
            provider.as_str(),
            root.display()
        )
    })?;
    validate_provider_root_path(&root)?;
    validate_provider_source_outside_data_root(data_root, &root).with_context(|| {
        format!(
            "{} history root {} must not overlap the ctx data root",
            provider.as_str(),
            root.display()
        )
    })?;
    Ok(root)
}

fn read_config_text(path: &Path) -> Result<String> {
    Ok(read_config_text_migrating_retired_controls_lock_held(path)?.unwrap_or_default())
}

fn validated_persisted_config(path: &Path, text: &str) -> Result<AppConfig> {
    let parsed = parse_toml_subset(text).with_context(|| format!("parse {}", path.display()))?;
    let mut config = AppConfig::default();
    config
        .apply_values(&parsed)
        .with_context(|| format!("load {}", path.display()))?;
    let data_root = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no data-root parent"))?;
    config
        .validate_provider_root_data_root(data_root)
        .with_context(|| format!("load {}", path.display()))?;
    Ok(config)
}

fn ensure_nested_table<'a>(
    document: &'a mut toml_edit::DocumentMut,
    parent: &str,
    child: &str,
) -> Result<&'a mut toml_edit::Table> {
    if document.as_table().get(parent).is_none() {
        document.as_table_mut().insert(parent, toml_edit::table());
    }
    let parent = document
        .as_table_mut()
        .get_mut(parent)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("configuration parent must be a table"))?;
    if parent.get(child).is_none() {
        parent.insert(child, toml_edit::table());
    }
    parent
        .get_mut(child)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("configuration child must be a table"))
}

fn configured_roots_table_mut(
    document: &mut toml_edit::DocumentMut,
) -> Result<&mut toml_edit::Table> {
    document
        .as_table_mut()
        .get_mut("sources")
        .and_then(toml_edit::Item::as_table_mut)
        .and_then(|sources| sources.get_mut("roots"))
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("sources.roots configuration must be a table"))
}

fn configured_root_table_mut<'a>(
    document: &'a mut toml_edit::DocumentMut,
    id: &str,
) -> Result<&'a mut toml_edit::Table> {
    configured_roots_table_mut(document)?
        .get_mut(id)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| {
            anyhow::anyhow!("provider root `{id}` disappeared during configuration update")
        })
}

fn persist_validated_document(path: &Path, document: toml_edit::DocumentMut) -> Result<()> {
    let updated = document.to_string();
    validated_persisted_config(path, &updated)
        .with_context(|| format!("validate updated {}", path.display()))?;
    write_config_durably(path, updated.as_bytes())
}

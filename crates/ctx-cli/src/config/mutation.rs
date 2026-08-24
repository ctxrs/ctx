use super::*;

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
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
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

#[derive(Debug, Clone)]
pub struct ProviderRootMutation {
    pub root: ProviderRootDefinition,
    pub changed: bool,
}

pub fn add_provider_root(
    data_root: &Path,
    id: &str,
    provider: CaptureProvider,
    root: &Path,
    group: Option<&str>,
) -> Result<ProviderRootMutation> {
    validate_root_selector("provider root name", id)?;
    validate_provider_root_support(provider)?;
    if let Some(group) = group {
        validate_root_selector("source group", group)?;
    }
    validate_provider_root_path(root)?;
    let metadata = fs::symlink_metadata(root)
        .with_context(|| format!("inspect provider home {}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "provider home must be an existing non-symlink directory: {}",
            root.display()
        );
    }
    let root = fs::canonicalize(root)
        .with_context(|| format!("canonicalize provider home {}", root.display()))?;
    validate_provider_root_path(&root)?;
    validate_provider_source_outside_data_root(data_root, &root).with_context(|| {
        format!(
            "provider home {} must not overlap the ctx data root",
            root.display()
        )
    })?;
    let desired = ProviderRootDefinition {
        id: id.to_owned(),
        provider,
        path: root,
        group: group.map(str::to_owned),
    };

    establish_private_data_root(data_root)?;
    let path = AppConfig::config_path(data_root);
    let _mutation_lock = ConfigMutationLock::acquire(&path)?;
    let text = read_config_text(&path)?;
    let current = validated_persisted_config(&path, &text)?;
    if let Some(existing) = current.provider_roots.get(id) {
        if existing == &desired {
            return Ok(ProviderRootMutation {
                root: existing.clone(),
                changed: false,
            });
        }
        bail!(
            "provider root `{id}` already exists with different settings; remove it before reusing the name"
        );
    }

    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parse {}", path.display()))?;
    let roots = ensure_nested_table(&mut document, "sources", "roots")?;
    let mut item = toml_edit::Table::new();
    item.insert("provider", toml_edit::value(provider.as_str()));
    item.insert("path", toml_edit::value(desired.path.display().to_string()));
    if let Some(group) = desired.group.as_deref() {
        item.insert("group", toml_edit::value(group));
    }
    roots.insert(id, toml_edit::Item::Table(item));
    persist_validated_document(&path, document)?;
    Ok(ProviderRootMutation {
        root: desired,
        changed: true,
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
    let roots = document
        .as_table_mut()
        .get_mut("sources")
        .and_then(toml_edit::Item::as_table_mut)
        .and_then(|sources| sources.get_mut("roots"))
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("sources.roots configuration must be a table"))?;
    if roots.remove(id).is_none() {
        bail!("provider root `{id}` disappeared during configuration update");
    }
    persist_validated_document(&path, document)?;
    Ok(ProviderRootMutation {
        root: existing,
        changed: true,
    })
}

fn read_config_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
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

fn persist_validated_document(path: &Path, document: toml_edit::DocumentMut) -> Result<()> {
    let updated = document.to_string();
    validated_persisted_config(path, &updated)
        .with_context(|| format!("validate updated {}", path.display()))?;
    write_config_durably(path, updated.as_bytes())
}

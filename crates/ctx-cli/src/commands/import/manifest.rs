use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::commands::import::catalog::{source_change_token, system_time_ms, SourceChangeEntry};
#[cfg(test)]
use ctx_history_capture::collect_provider_source_files;
use ctx_history_capture::{
    observe_ordinary_file, observe_sqlite_source_generation, pace_current_disk_io,
    pace_current_filesystem_operation, ProviderImportDependency, ProviderImportUnitGrouping,
    ProviderImportUnitOwner, ProviderImportUnitSpec,
};

pub(crate) struct PersistedSourceImportObservation {
    pub(crate) inventory_generation: u64,
    pub(crate) pending_files: Vec<SourceImportFile>,
}

pub(crate) struct SourceImportObservationOutcome<'a> {
    pub(crate) file: &'a SourceImportFile,
    pub(crate) status: CatalogIndexedStatus,
    pub(crate) error: Option<&'a str>,
}

pub(crate) fn persist_new_source_import_observation(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
) -> Result<PersistedSourceImportObservation> {
    persist_source_import_observation_with_outcomes(store, source, files, &[])
}

pub(crate) fn persist_source_import_observation_with_outcomes(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
    outcomes: &[SourceImportObservationOutcome<'_>],
) -> Result<PersistedSourceImportObservation> {
    persist_source_import_observation_with_outcomes_inner(store, source, files, outcomes, || {})
}

#[cfg(test)]
pub(crate) fn persist_source_import_observation_with_outcomes_and_hook(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
    outcomes: &[SourceImportObservationOutcome<'_>],
    before_outcomes: impl FnOnce(),
) -> Result<PersistedSourceImportObservation> {
    persist_source_import_observation_with_outcomes_inner(
        store,
        source,
        files,
        outcomes,
        before_outcomes,
    )
}

fn persist_source_import_observation_with_outcomes_inner(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
    outcomes: &[SourceImportObservationOutcome<'_>],
    before_outcomes: impl FnOnce(),
) -> Result<PersistedSourceImportObservation> {
    let source_root = persisted_import_identity(&source.path, "source root")?;
    store.begin_immediate_batch()?;
    let persist = (|| -> Result<PersistedSourceImportObservation> {
        let inventory_generation =
            store.allocate_source_import_inventory_generation(source.provider, source_root)?;
        persist_source_import_files_in_batch(
            store,
            source,
            inventory_generation,
            files,
            source_root,
        )?;
        before_outcomes();
        for outcome in outcomes {
            pace_current_disk_io(
                256u64
                    .saturating_add(outcome.file.source_format.len() as u64)
                    .saturating_add(outcome.file.source_root.len() as u64)
                    .saturating_add(outcome.file.source_path.len() as u64)
                    .saturating_add(serde_json::to_vec(&outcome.file.metadata)?.len() as u64),
            );
            let changed = store.record_source_import_file_result(
                outcome.file.provider,
                SourceImportFileIndexUpdate {
                    source_root: &outcome.file.source_root,
                    source_path: &outcome.file.source_path,
                    file_size_bytes: outcome.file.file_size_bytes,
                    file_modified_at_ms: outcome.file.file_modified_at_ms,
                    import_revision: outcome.file.import_revision,
                    inventory_generation,
                    metadata: &outcome.file.metadata,
                    indexed_at_ms: utc_now().timestamp_millis(),
                },
                outcome.status,
                outcome.error,
            )?;
            if changed != 1 {
                return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                    "current source observation outcome did not update exactly one inventory row",
                )));
            }
        }
        if !store.complete_source_import_inventory_generation(
            source.provider,
            source_root,
            inventory_generation,
        )? {
            return Err(anyhow::Error::new(CaptureError::InventorySuperseded));
        }
        let pending_files = store.list_pending_source_import_files(source.provider, source_root)?;
        Ok(PersistedSourceImportObservation {
            inventory_generation,
            pending_files,
        })
    })();
    match persist {
        Ok(observation) => {
            store.commit_batch()?;
            Ok(observation)
        }
        Err(err) => {
            let _ = store.rollback_batch();
            Err(err)
        }
    }
}

#[cfg(test)]
pub(crate) fn persist_source_import_files(
    store: &Store,
    source: &SourceInfo,
    inventory_generation: u64,
    files: &[SourceImportFile],
) -> Result<()> {
    let source_root = persisted_import_identity(&source.path, "source root")?;
    store.begin_immediate_batch()?;
    let persist = persist_source_import_files_in_batch(
        store,
        source,
        inventory_generation,
        files,
        source_root,
    );
    match persist {
        Ok(()) => store.commit_batch()?,
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }
    Ok(())
}

pub(crate) fn persist_source_import_files_page(
    store: &Store,
    inventory_generation: u64,
    files: &[SourceImportFile],
) -> Result<()> {
    if files.len() > 64 {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "source import inventory page exceeds its internal row limit",
        )));
    }
    store.begin_immediate_batch()?;
    let persisted = store.upsert_source_import_files_with_pacing(
        inventory_generation,
        files,
        pace_current_disk_io,
    );
    match persisted {
        Ok(_) => store.commit_batch()?,
        Err(error) => {
            let _ = store.rollback_batch();
            return Err(error.into());
        }
    }
    Ok(())
}

fn persist_source_import_files_in_batch(
    store: &Store,
    source: &SourceInfo,
    inventory_generation: u64,
    files: &[SourceImportFile],
    source_root: &str,
) -> Result<()> {
    let current_paths = files
        .iter()
        .map(|file| file.source_path.clone())
        .collect::<Vec<_>>();
    store.upsert_source_import_files_with_pacing(
        inventory_generation,
        files,
        pace_current_disk_io,
    )?;
    store.mark_source_import_missing_paths_stale_with_pacing(
        source.provider,
        source_root,
        &current_paths,
        utc_now().timestamp_millis(),
        inventory_generation,
        pace_current_disk_io,
    )?;
    Ok(())
}

pub(crate) fn source_uses_import_file_manifest(source: &SourceInfo) -> bool {
    source.import_unit.uses_file_manifest()
}

#[cfg(test)]
pub(crate) fn collect_source_import_files(source: &SourceInfo) -> Result<Vec<SourceImportFile>> {
    let source_root = persisted_import_identity(&source.path, "source root")?.to_owned();
    let units = collect_source_import_units(source)?;
    let observed_at_ms = utc_now().timestamp_millis();
    let mut files = Vec::with_capacity(units.len());
    for unit in units {
        files.push(observe_collected_source_import_unit(
            source,
            &source_root,
            &unit,
            observed_at_ms,
        )?);
    }
    Ok(files)
}

pub(crate) fn manifest_inventory_path_candidate(
    source: &SourceInfo,
    path: &Path,
) -> Option<(Vec<u8>, u64)> {
    let ProviderImportUnitSpec::PerFile {
        owner, grouping, ..
    } = source.import_unit
    else {
        return None;
    };
    if !import_unit_owner_matches(owner, &source.path, path) {
        return None;
    }
    let (group_key, rank) = match grouping {
        ProviderImportUnitGrouping::Each => (path.as_os_str().as_encoded_bytes().to_vec(), 0),
        ProviderImportUnitGrouping::FirstPerDirectory => {
            let directory = path.parent().unwrap_or(path);
            (
                directory.as_os_str().as_encoded_bytes().to_vec(),
                import_owner_rank(owner, path) as u64,
            )
        }
        ProviderImportUnitGrouping::AntigravitySession => {
            let rank = u64::from(
                path.file_name().and_then(|name| name.to_str()) != Some("transcript_full.jsonl"),
            );
            (antigravity_session_key_from_path(path).into_bytes(), rank)
        }
    };
    Some((group_key, rank))
}

pub(crate) fn observe_source_import_paths_page(
    source: &SourceInfo,
    paths: Vec<PathBuf>,
) -> Result<Vec<SourceImportFile>> {
    if paths.len() > 64 {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "manifest inventory page exceeds its internal path limit",
        )));
    }
    let ProviderImportUnitSpec::PerFile { dependencies, .. } = source.import_unit else {
        return Ok(Vec::new());
    };
    let source_root = persisted_import_identity(&source.path, "source root")?.to_owned();
    let observed_at_ms = utc_now().timestamp_millis();
    paths
        .into_iter()
        .map(|path| {
            let unit = collected_import_unit(path, dependencies)?;
            observe_collected_source_import_unit(source, &source_root, &unit, observed_at_ms)
        })
        .collect()
}

pub(crate) fn observe_selected_source_import_file(
    source: &SourceInfo,
    source_path: &str,
) -> Result<Option<SourceImportFile>> {
    let ProviderImportUnitSpec::PerFile {
        owner,
        dependencies,
        ..
    } = source.import_unit
    else {
        return Err(anyhow!(
            "selected import file does not belong to a manifested source"
        ));
    };
    let path = PathBuf::from(source_path);
    let belongs_to_source = path == source.path || path.starts_with(&source.path);
    if !belongs_to_source || !import_unit_owner_matches(owner, &source.path, &path) {
        return Err(anyhow!(
            "selected import unit is outside its manifested source: {}",
            path.display()
        ));
    }
    pace_current_filesystem_operation(path.as_os_str().len() as u64);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(anyhow!(
                "import unit owner is not a regular file: {}",
                path.display()
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("stat import unit owner {}", path.display()))
        }
    }
    let source_root = persisted_import_identity(&source.path, "source root")?;
    let unit = collected_import_unit(path, dependencies)?;
    observe_collected_source_import_unit(source, source_root, &unit, utc_now().timestamp_millis())
        .map(Some)
}

pub(crate) fn same_source_import_observation(
    left: &SourceImportFile,
    right: &SourceImportFile,
) -> bool {
    left.provider == right.provider
        && left.source_format == right.source_format
        && left.source_root == right.source_root
        && left.source_path == right.source_path
        && left.file_size_bytes == right.file_size_bytes
        && left.file_modified_at_ms == right.file_modified_at_ms
        && left.import_revision == right.import_revision
        && left.metadata == right.metadata
}

fn observe_collected_source_import_unit(
    source: &SourceInfo,
    source_root: &str,
    unit: &CollectedImportUnit,
    observed_at_ms: i64,
) -> Result<SourceImportFile> {
    let owner_identity = persisted_import_identity(&unit.owner, "import unit owner")?;
    let fingerprint_base = if source.path.is_dir() {
        source.path.as_path()
    } else {
        source.path.parent().unwrap_or(source.path.as_path())
    };
    let fingerprint = import_unit_fingerprint(fingerprint_base, unit)?;
    let dependency_paths = fingerprint
        .dependencies
        .iter()
        .filter(|path| *path != &unit.owner)
        .map(|path| import_unit_path_label(fingerprint_base, path))
        .collect::<Vec<_>>();
    let absent_dependency_paths = fingerprint
        .absent_dependencies
        .iter()
        .map(|path| import_unit_path_label(fingerprint_base, path))
        .collect::<Vec<_>>();
    Ok(SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_root.to_owned(),
        source_path: owner_identity.to_owned(),
        file_size_bytes: fingerprint.owner_len,
        file_modified_at_ms: system_time_ms(fingerprint.owner_modified_at),
        import_revision: source.import_revision,
        observed_at_ms,
        metadata: json!({
            "inventory_unit": "logical_import_unit",
            "change_token_v1": hex_change_token(fingerprint.change_token),
            "dependencies": dependency_paths,
            "absent_dependencies": absent_dependency_paths,
        }),
    })
}

struct CollectedImportUnit {
    owner: PathBuf,
    dependencies: Vec<PathBuf>,
    absence_watches: Vec<PathBuf>,
    sqlite_sidecars: bool,
}

#[cfg(test)]
fn collect_source_import_units(source: &SourceInfo) -> Result<Vec<CollectedImportUnit>> {
    let ProviderImportUnitSpec::PerFile {
        owner,
        grouping,
        dependencies,
    } = source.import_unit
    else {
        return Ok(Vec::new());
    };
    pace_current_filesystem_operation(source.path.as_os_str().len() as u64);
    let metadata = fs::symlink_metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "symlinked provider transcript roots are rejected: {}",
            source.path.display()
        ));
    }
    if metadata.file_type().is_file() {
        return Ok(
            if import_unit_owner_matches(owner, &source.path, &source.path) {
                vec![collected_import_unit(source.path.clone(), dependencies)?]
            } else {
                Vec::new()
            },
        );
    }
    if !metadata.file_type().is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = collect_provider_source_files(&source.path).with_context(|| {
        format!(
            "inventory import source files under {}",
            source.path.display()
        )
    })?;
    paths.retain(|path| import_unit_owner_matches(owner, &source.path, path));
    paths = preferred_source_import_paths(grouping, owner, paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| collected_import_unit(path, dependencies))
        .collect()
}

fn collected_import_unit(
    owner: PathBuf,
    dependency_specs: &[ProviderImportDependency],
) -> Result<CollectedImportUnit> {
    let mut dependencies = BTreeSet::from([owner.clone()]);
    let mut absence_watches = BTreeSet::new();
    let sqlite_sidecars = dependency_specs.contains(&ProviderImportDependency::SqliteSidecars);
    for dependency in dependency_specs {
        collect_import_unit_dependency(
            &owner,
            *dependency,
            &mut dependencies,
            &mut absence_watches,
        )?;
    }
    Ok(CollectedImportUnit {
        owner,
        dependencies: dependencies.into_iter().collect(),
        absence_watches: absence_watches.into_iter().collect(),
        sqlite_sidecars,
    })
}

fn collect_import_unit_dependency(
    owner: &Path,
    dependency: ProviderImportDependency,
    paths: &mut BTreeSet<PathBuf>,
    absence_watches: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    match dependency {
        ProviderImportDependency::SqliteSidecars => {
            // SQLite dependencies are observed as one stable generation when
            // the fingerprint is built, so a checkpoint race cannot split them.
        }
        ProviderImportDependency::SiblingFile(name) => {
            if let Some(parent) = owner.parent() {
                collect_existing_import_unit_dependency(parent.join(name), paths, absence_watches)?;
            }
        }
        ProviderImportDependency::AncestorFile { levels, name } => {
            let mut directory = owner.parent();
            for _ in 0..levels {
                directory = directory.and_then(Path::parent);
            }
            if let Some(directory) = directory {
                collect_existing_import_unit_dependency(
                    directory.join(name),
                    paths,
                    absence_watches,
                )?;
            }
        }
        ProviderImportDependency::NearestAncestorFile(name) => {
            let mut directory = owner.parent();
            while let Some(candidate_dir) = directory {
                let candidate = candidate_dir.join(name);
                pace_current_filesystem_operation(candidate.as_os_str().len() as u64);
                match fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        paths.insert(candidate);
                        break;
                    }
                    Ok(_) => {
                        return Err(anyhow!(
                            "import unit dependency is not a regular file: {}",
                            candidate.display()
                        ))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        absence_watches.insert(candidate);
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("stat import unit dependency {}", candidate.display())
                        })
                    }
                }
                directory = candidate_dir.parent();
            }
        }
    }
    Ok(())
}

fn collect_existing_import_unit_dependency(
    path: PathBuf,
    paths: &mut BTreeSet<PathBuf>,
    absence_watches: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    pace_current_filesystem_operation(path.as_os_str().len() as u64);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            paths.insert(path);
            Ok(())
        }
        Ok(_) => Err(anyhow!(
            "import unit dependency is not a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            absence_watches.insert(path);
            Ok(())
        }
        Err(error) => {
            Err(error).with_context(|| format!("stat import unit dependency {}", path.display()))
        }
    }
}

fn preferred_source_import_paths(
    grouping: ProviderImportUnitGrouping,
    owner: ProviderImportUnitOwner,
    paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    match grouping {
        ProviderImportUnitGrouping::Each => paths,
        ProviderImportUnitGrouping::FirstPerDirectory => {
            first_import_path_per_directory(owner, paths)
        }
        ProviderImportUnitGrouping::AntigravitySession => antigravity_preferred_import_paths(paths),
    }
}

fn first_import_path_per_directory(
    owner: ProviderImportUnitOwner,
    paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut by_directory = BTreeMap::<PathBuf, PathBuf>::new();
    for path in paths {
        let directory = path.parent().unwrap_or(path.as_path()).to_path_buf();
        let replace = by_directory
            .get(&directory)
            .map(|current| import_owner_rank(owner, &path) < import_owner_rank(owner, current))
            .unwrap_or(true);
        if replace {
            by_directory.insert(directory, path);
        }
    }
    by_directory.into_values().collect()
}

fn import_owner_rank(owner: ProviderImportUnitOwner, path: &Path) -> usize {
    let ProviderImportUnitOwner::FileNames { names, .. } = owner else {
        return 0;
    };
    let file_name = path.file_name().and_then(|name| name.to_str());
    names
        .iter()
        .position(|candidate| Some(*candidate) == file_name)
        .unwrap_or(names.len())
}

fn antigravity_preferred_import_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_session: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in paths {
        let session = antigravity_session_key_from_path(&path);
        let prefer_new =
            path.file_name().and_then(|name| name.to_str()) == Some("transcript_full.jsonl");
        let replace = by_session
            .get(&session)
            .map(|current| {
                prefer_new
                    && current.file_name().and_then(|name| name.to_str())
                        != Some("transcript_full.jsonl")
            })
            .unwrap_or(true);
        if replace {
            by_session.insert(session, path);
        }
    }
    by_session.into_values().collect()
}

fn antigravity_session_key_from_path(path: &Path) -> String {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect::<Vec<_>>();
    components
        .windows(2)
        .find_map(|window| {
            (window[0] == "brain" && !window[1].trim().is_empty()).then(|| window[1].clone())
        })
        .or_else(|| {
            components.windows(2).find_map(|window| {
                (window[1] == ".system_generated" && !window[0].trim().is_empty())
                    .then(|| window[0].clone())
            })
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| path.display().to_string())
}

fn import_unit_owner_matches(owner: ProviderImportUnitOwner, source: &Path, path: &Path) -> bool {
    match owner {
        ProviderImportUnitOwner::SourceFile => path == source,
        ProviderImportUnitOwner::FileNames {
            names,
            required_component,
        } => {
            names.contains(
                &path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(""),
            ) && required_component
                .map(|component| path_has_component(path, component))
                .unwrap_or(true)
        }
        ProviderImportUnitOwner::Extensions {
            extensions,
            required_component,
            excluded_names,
        } => {
            extensions.contains(&path.extension().and_then(|ext| ext.to_str()).unwrap_or(""))
                && !excluded_names.contains(
                    &path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(""),
                )
                && required_component
                    .map(|component| path_has_component(path, component))
                    .unwrap_or(true)
        }
    }
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

struct ImportUnitFingerprint {
    change_token: [u8; 32],
    dependencies: Vec<PathBuf>,
    absent_dependencies: Vec<PathBuf>,
    owner_len: u64,
    owner_modified_at: SystemTime,
}

fn import_unit_fingerprint(
    base: &Path,
    unit: &CollectedImportUnit,
) -> Result<ImportUnitFingerprint> {
    let mut entries = Vec::with_capacity(unit.dependencies.len() + unit.absence_watches.len() + 2);
    let mut dependencies = unit.dependencies.iter().cloned().collect::<BTreeSet<_>>();
    let (owner_len, owner_modified_at) = if unit.sqlite_sidecars {
        let generation = observe_sqlite_source_generation(&unit.owner)
            .with_context(|| format!("observe SQLite import unit {}", unit.owner.display()))?;
        for file in generation.files() {
            dependencies.insert(file.path().to_path_buf());
            entries.push(SourceChangeEntry::from_sqlite_observed(base, file));
        }
        (generation.main().len(), generation.main().modified_at())
    } else {
        let observation = observe_ordinary_file(&unit.owner)
            .with_context(|| format!("observe import unit owner {}", unit.owner.display()))?;
        entries.push(SourceChangeEntry::from_observation(
            base,
            &unit.owner,
            &observation,
        ));
        (observation.len(), observation.modified_at())
    };
    for path in unit.dependencies.iter().filter(|path| *path != &unit.owner) {
        let observation = observe_ordinary_file(path)
            .with_context(|| format!("observe import unit dependency {}", path.display()))?;
        entries.push(SourceChangeEntry::from_observation(
            base,
            path,
            &observation,
        ));
    }
    let observed_absences = unit
        .absence_watches
        .iter()
        .filter(|path| !dependencies.contains(*path))
        .map(|path| absence_watch_change_entry(base, path).map(|entry| (path.clone(), entry)))
        .collect::<Result<Vec<_>>>()?;
    let (absent_dependencies, absence_entries): (Vec<_>, Vec<_>) =
        observed_absences.into_iter().unzip();
    entries.extend(absence_entries);
    Ok(ImportUnitFingerprint {
        change_token: source_change_token(entries),
        dependencies: dependencies.into_iter().collect(),
        absent_dependencies,
        owner_len,
        owner_modified_at,
    })
}

fn absence_watch_change_entry(base: &Path, path: &Path) -> Result<SourceChangeEntry> {
    pace_current_filesystem_operation(path.as_os_str().len() as u64);
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "optional import dependency appeared while it was being observed: {}",
                    path.display()
                ),
            )
            .into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("stat optional import dependency {}", path.display()))
        }
    }
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "optional import dependency has no parent directory: {}",
            path.display()
        )
    })?;
    pace_current_filesystem_operation(parent.as_os_str().len() as u64);
    let parent_metadata = fs::symlink_metadata(parent).with_context(|| {
        format!(
            "stat optional import dependency parent {}",
            parent.display()
        )
    })?;
    if !parent_metadata.file_type().is_dir() {
        return Err(anyhow!(
            "optional import dependency parent is not a directory: {}",
            parent.display()
        ));
    }
    pace_current_filesystem_operation(path.as_os_str().len() as u64);
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "optional import dependency appeared while it was being observed: {}",
                    path.display()
                ),
            )
            .into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("restat optional import dependency {}", path.display()))
        }
    }
    let mut token_path = path.as_os_str().to_owned();
    token_path.push(".ctx-absence-watch-v1");
    Ok(SourceChangeEntry::from_metadata(
        base,
        &PathBuf::from(token_path),
        &parent_metadata,
    ))
}

pub(crate) fn persisted_import_identity<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| {
        anyhow!(
            "{label} cannot be persisted because it is not valid UTF-8: {}",
            path.display()
        )
    })
}

fn import_unit_path_label(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn hex_change_token(token: [u8; 32]) -> String {
    token
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

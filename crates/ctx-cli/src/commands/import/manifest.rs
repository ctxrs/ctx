use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::commands::import::catalog::{source_change_token, system_time_ms, SourceChangeEntry};
use ctx_history_capture::{
    observe_sqlite_source_generation, ProviderImportDependency, ProviderImportUnitGrouping,
    ProviderImportUnitOwner, ProviderImportUnitSpec,
};

pub(crate) fn persist_source_import_files(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
) -> Result<()> {
    let source_root = persisted_import_identity(&source.path, "source root")?;
    let current_paths = files
        .iter()
        .map(|file| file.source_path.clone())
        .collect::<Vec<_>>();
    let observed_at_ms = utc_now().timestamp_millis();
    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        store.upsert_source_import_files(files)?;
        store.mark_source_import_missing_paths_stale(
            source.provider,
            source_root,
            &current_paths,
            observed_at_ms,
        )?;
        Ok(())
    })();
    match persist {
        Ok(()) => store.commit_batch()?,
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }
    Ok(())
}

pub(crate) fn source_uses_import_file_manifest(source: &SourceInfo) -> bool {
    source.import_unit.uses_file_manifest()
}

pub(crate) fn collect_source_import_files(source: &SourceInfo) -> Result<Vec<SourceImportFile>> {
    let source_root = persisted_import_identity(&source.path, "source root")?.to_owned();
    let units = collect_source_import_units(source)?;
    let observed_at_ms = utc_now().timestamp_millis();
    let mut files = Vec::with_capacity(units.len());
    for unit in units {
        let owner_identity = persisted_import_identity(&unit.owner, "import unit owner")?;
        let fingerprint_base = if source.path.is_dir() {
            source.path.as_path()
        } else {
            source.path.parent().unwrap_or(source.path.as_path())
        };
        let fingerprint = import_unit_fingerprint(fingerprint_base, &unit)?;
        let dependency_paths = fingerprint
            .dependencies
            .iter()
            .filter(|path| *path != &unit.owner)
            .map(|path| import_unit_path_label(fingerprint_base, path))
            .collect::<Vec<_>>();
        files.push(SourceImportFile {
            provider: source.provider,
            source_format: source.source_format.to_owned(),
            source_root: source_root.clone(),
            source_path: owner_identity.to_owned(),
            file_size_bytes: fingerprint.owner_len,
            file_modified_at_ms: system_time_ms(fingerprint.owner_modified_at),
            observed_at_ms,
            metadata: json!({
                "inventory_unit": "logical_import_unit",
                "change_token_v1": hex_change_token(fingerprint.change_token),
                "dependencies": dependency_paths,
            }),
        });
    }
    Ok(files)
}

pub(crate) fn collect_source_import_paths(source: &SourceInfo) -> Result<Vec<PathBuf>> {
    Ok(collect_source_import_units(source)?
        .into_iter()
        .map(|unit| unit.owner)
        .collect())
}

struct CollectedImportUnit {
    owner: PathBuf,
    dependencies: Vec<PathBuf>,
    sqlite_sidecars: bool,
}

fn collect_source_import_units(source: &SourceInfo) -> Result<Vec<CollectedImportUnit>> {
    let ProviderImportUnitSpec::PerFile {
        owner,
        grouping,
        dependencies,
    } = source.import_unit
    else {
        return Ok(Vec::new());
    };
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

    let mut paths = Vec::new();
    let mut stack = vec![source.path.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("read import source directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("read import source entry under {}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat import source entry {}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && import_unit_owner_matches(owner, &source.path, &path) {
                paths.push(path);
            }
        }
    }
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
    let sqlite_sidecars = dependency_specs.contains(&ProviderImportDependency::SqliteSidecars);
    for dependency in dependency_specs {
        collect_import_unit_dependency(&owner, *dependency, &mut dependencies)?;
    }
    Ok(CollectedImportUnit {
        owner,
        dependencies: dependencies.into_iter().collect(),
        sqlite_sidecars,
    })
}

fn collect_import_unit_dependency(
    owner: &Path,
    dependency: ProviderImportDependency,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    match dependency {
        ProviderImportDependency::SqliteSidecars => {
            // SQLite dependencies are observed as one stable generation when
            // the fingerprint is built, so a checkpoint race cannot split them.
        }
        ProviderImportDependency::SiblingFile(name) => {
            if let Some(parent) = owner.parent() {
                collect_existing_import_unit_dependency(parent.join(name), paths)?;
            }
        }
        ProviderImportDependency::AncestorFile { levels, name } => {
            let mut directory = owner.parent();
            for _ in 0..levels {
                directory = directory.and_then(Path::parent);
            }
            if let Some(directory) = directory {
                collect_existing_import_unit_dependency(directory.join(name), paths)?;
            }
        }
        ProviderImportDependency::NearestAncestorFile(name) => {
            let mut directory = owner.parent();
            while let Some(candidate_dir) = directory {
                let candidate = candidate_dir.join(name);
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
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
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
) -> Result<()> {
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            paths.insert(path);
            Ok(())
        }
        Ok(_) => Err(anyhow!(
            "import unit dependency is not a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
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
    owner_len: u64,
    owner_modified_at: SystemTime,
}

fn import_unit_fingerprint(
    base: &Path,
    unit: &CollectedImportUnit,
) -> Result<ImportUnitFingerprint> {
    let mut entries = Vec::with_capacity(unit.dependencies.len() + 2);
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
        let metadata = fs::symlink_metadata(&unit.owner)
            .with_context(|| format!("stat import unit owner {}", unit.owner.display()))?;
        if !metadata.file_type().is_file() {
            return Err(anyhow!(
                "import unit owner is not a regular file: {}",
                unit.owner.display()
            ));
        }
        entries.push(SourceChangeEntry::from_metadata(
            base,
            &unit.owner,
            &metadata,
        ));
        (metadata.len(), metadata.modified().unwrap_or(UNIX_EPOCH))
    };
    for path in unit.dependencies.iter().filter(|path| *path != &unit.owner) {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("stat import unit dependency {}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(anyhow!(
                "import unit dependency is not a regular file: {}",
                path.display()
            ));
        }
        entries.push(SourceChangeEntry::from_metadata(base, path, &metadata));
    }
    Ok(ImportUnitFingerprint {
        change_token: source_change_token(entries),
        dependencies: dependencies.into_iter().collect(),
        owner_len,
        owner_modified_at,
    })
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

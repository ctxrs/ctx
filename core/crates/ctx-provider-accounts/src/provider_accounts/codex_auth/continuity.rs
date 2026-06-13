use super::*;
use crate::provider_accounts::paths::{
    codex_brokers_root, validate_codex_brokers_root_before_broker_access, ProviderStorageChild,
    ProviderStorageChildKind, CODEX_CONTINUITY_STATE_CHILDREN,
};
use fs2::FileExt;
use std::{collections::HashMap, fs, io};

const CODEX_CONTINUITY_REPAIR_LOCK_FILE: &str = ".ctx-continuity-migration.lock";
const CODEX_CONTINUITY_RUNTIME_LOCK_FILE: &str = ".ctx-continuity-runtime.lock";

pub(crate) async fn expose_legacy_codex_state_to_broker_home(
    data_root: &Path,
    broker_home: &Path,
) -> Result<()> {
    let original_data_root = data_root.to_path_buf();
    let canonical_data_root =
        fs::canonicalize(data_root).unwrap_or_else(|_| original_data_root.clone());
    let canonical_broker_home = broker_home
        .strip_prefix(&original_data_root)
        .map(|relative| canonical_data_root.join(relative))
        .unwrap_or_else(|_| broker_home.to_path_buf());
    let data_root_alias = original_data_root.as_path();
    let data_root = canonical_data_root.as_path();
    let broker_home = canonical_broker_home.as_path();
    let shared_home = codex_runtime_home(data_root);
    validate_codex_runtime_home_path_before_lock(data_root, data_root_alias, &shared_home)?;
    let _repair_lock = acquire_codex_continuity_repair_lock(&shared_home).await?;
    let Some(_runtime_locks) = try_acquire_codex_continuity_runtime_locks(
        data_root,
        data_root_alias,
        broker_home,
        &shared_home,
    )?
    else {
        ensure_broker_continuity_ready_before_deferred_launch(
            data_root,
            data_root_alias,
            broker_home,
            &shared_home,
        )?;
        return Ok(());
    };
    ctx_fs::permissions::ensure_private_dir(broker_home).await?;
    ctx_fs::permissions::ensure_private_dir(&shared_home).await?;
    let legacy_home = legacy_codex_runtime_home(data_root);
    if legacy_home != shared_home && legacy_home != broker_home {
        merge_legacy_codex_state_home(data_root, data_root_alias, &legacy_home, &shared_home)
            .await?;
    }
    if shared_home != broker_home {
        expose_legacy_codex_state_from_home(data_root_alias, &shared_home, broker_home).await?;
    }
    Ok(())
}

fn try_acquire_codex_continuity_runtime_locks(
    data_root: &Path,
    data_root_alias: &Path,
    broker_home: &Path,
    shared_home: &Path,
) -> Result<Option<Vec<std::fs::File>>> {
    let mut locks = Vec::new();
    let mut locked_homes = Vec::new();
    if !try_acquire_codex_runtime_home_lock(
        data_root,
        data_root_alias,
        broker_home,
        &mut locked_homes,
        &mut locks,
    )? {
        return Ok(None);
    }

    let brokers_root = codex_brokers_root(data_root);
    validate_codex_brokers_root_before_broker_access(data_root)?;
    match fs::read_dir(&brokers_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.with_context(|| {
                    format!(
                        "reading Codex broker root entry under {}",
                        brokers_root.display()
                    )
                })?;
                let entry_type = entry.file_type().with_context(|| {
                    format!(
                        "checking Codex broker root entry {} before lock scan",
                        entry.path().display()
                    )
                })?;
                if entry_type.is_symlink() {
                    anyhow::bail!(
                        "Codex broker root entry {} must not be a symlink before lock acquisition",
                        entry.path().display()
                    );
                }
                if !entry_type.is_dir() {
                    continue;
                }
                let home = entry.path().join("home");
                if !codex_runtime_home_is_lock_scan_candidate(data_root, data_root_alias, &home)? {
                    continue;
                }
                if !try_acquire_codex_runtime_home_lock(
                    data_root,
                    data_root_alias,
                    &home,
                    &mut locked_homes,
                    &mut locks,
                )? {
                    return Ok(None);
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!("reading Codex broker roots at {}", brokers_root.display())
            });
        }
    }

    if shared_home != broker_home
        && !try_acquire_codex_runtime_home_lock(
            data_root,
            data_root_alias,
            shared_home,
            &mut locked_homes,
            &mut locks,
        )?
    {
        return Ok(None);
    }

    let legacy_home = legacy_codex_runtime_home(data_root);
    if legacy_home.as_path() != shared_home
        && legacy_home.as_path() != broker_home
        && codex_runtime_home_is_lock_scan_candidate(data_root, data_root_alias, &legacy_home)?
        && !try_acquire_codex_runtime_home_lock(
            data_root,
            data_root_alias,
            &legacy_home,
            &mut locked_homes,
            &mut locks,
        )?
    {
        return Ok(None);
    }

    Ok(Some(locks))
}

fn try_acquire_codex_runtime_home_lock(
    data_root: &Path,
    data_root_alias: &Path,
    home: &Path,
    locked_homes: &mut Vec<PathBuf>,
    locks: &mut Vec<std::fs::File>,
) -> Result<bool> {
    if locked_homes.iter().any(|locked_home| locked_home == home) {
        return Ok(true);
    }
    validate_codex_runtime_home_path_before_lock(data_root, data_root_alias, home)?;
    let Some(lock) = try_acquire_codex_continuity_runtime_lock(home)? else {
        return Ok(false);
    };
    locked_homes.push(home.to_path_buf());
    locks.push(lock);
    Ok(true)
}

fn codex_runtime_home_is_lock_scan_candidate(
    data_root: &Path,
    data_root_alias: &Path,
    home: &Path,
) -> Result<bool> {
    if path_has_symlink_component_below(data_root, data_root_alias, home)? {
        anyhow::bail!(
            "Codex runtime home path {} must not contain a symlink component before lock acquisition",
            home.display()
        );
    }
    match fs::symlink_metadata(home) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                anyhow::bail!(
                    "Codex runtime home path {} must not be a symlink before lock acquisition",
                    home.display()
                );
            }
            Ok(metadata.file_type().is_dir())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| {
            format!(
                "checking Codex runtime home path {} before lock acquisition",
                home.display()
            )
        }),
    }
}

fn try_acquire_codex_continuity_runtime_lock(home: &Path) -> Result<Option<std::fs::File>> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating Codex runtime home at {}", home.display()))?;
    let lock_path = home.join(CODEX_CONTINUITY_RUNTIME_LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "opening Codex continuity runtime lock {}",
                lock_path.display()
            )
        })?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(err) => Err(err).with_context(|| {
            format!(
                "locking Codex continuity runtime lock {}",
                lock_path.display()
            )
        }),
    }
}

pub fn acquire_codex_runtime_continuity_lock_from_env(
    env: &HashMap<String, String>,
) -> Result<Option<std::fs::File>> {
    let Some(codex_home) = env
        .get("CODEX_HOME")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    Ok(Some(acquire_codex_runtime_continuity_lock(&codex_home)?))
}

fn acquire_codex_runtime_continuity_lock(home: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating Codex runtime home at {}", home.display()))?;
    let lock_path = home.join(CODEX_CONTINUITY_RUNTIME_LOCK_FILE);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "opening Codex continuity runtime lock {}",
                lock_path.display()
            )
        })?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(file),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
            anyhow::bail!(
                "Codex home {} is undergoing continuity migration. Retry after launch preparation finishes.",
                home.display()
            )
        }
        Err(err) => {
            Err(err).with_context(|| format!("locking Codex runtime home {}", home.display()))
        }
    }
}

fn ensure_broker_continuity_ready_before_deferred_launch(
    data_root: &Path,
    data_root_alias: &Path,
    broker_home: &Path,
    shared_home: &Path,
) -> Result<()> {
    match fs::symlink_metadata(broker_home) {
        Ok(metadata) => {
            validate_codex_state_source_home(data_root, data_root_alias, broker_home, &metadata)?
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            anyhow::bail!(
                "Codex continuity repair is blocked by an active runtime before broker home {} was linked; close active Codex sessions and retry",
                broker_home.display()
            );
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "checking Codex broker home {} before deferred continuity repair",
                    broker_home.display()
                )
            });
        }
    }

    for child in CODEX_CONTINUITY_STATE_CHILDREN {
        let dest = broker_home.join(child.name);
        let metadata = match fs::symlink_metadata(&dest) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                anyhow::bail!(
                    "Codex continuity child {} is missing while repair is blocked by an active runtime; close active Codex sessions and retry",
                    dest.display()
                );
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "checking broker Codex continuity child {} before deferred repair",
                        dest.display()
                    )
                });
            }
        };

        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&dest)
                .with_context(|| format!("reading broker Codex state link {}", dest.display()))?;
            let source = shared_home.join(child.name);
            if symlink_target_points_to_source(data_root, data_root_alias, &source, &dest, &target)
            {
                let source_metadata = fs::metadata(&source).with_context(|| {
                    format!(
                        "checking shared Codex continuity child {} before deferred repair",
                        source.display()
                    )
                })?;
                validate_codex_state_child_metadata(&source, &source_metadata, child)?;
                continue;
            }
            let resolved_target = resolve_symlink_target(&dest, &target)?;
            ensure_stale_codex_state_link_target_is_safe(
                data_root,
                data_root_alias,
                &resolved_target,
                child,
            )?;
            anyhow::bail!(
                "Codex continuity child {} points at stale state that requires repair while repair is blocked by an active runtime; close active Codex sessions and retry",
                dest.display()
            );
        }

        validate_codex_state_child_metadata(&dest, &metadata, child)?;
        anyhow::bail!(
            "Codex continuity child {} requires repair while repair is blocked by an active runtime; close active Codex sessions and retry",
            dest.display()
        );
    }

    Ok(())
}

pub(crate) async fn expose_legacy_codex_state_from_home(
    data_root: &Path,
    legacy_home: &Path,
    broker_home: &Path,
) -> Result<()> {
    let original_data_root = data_root.to_path_buf();
    let canonical_data_root =
        fs::canonicalize(data_root).unwrap_or_else(|_| original_data_root.clone());
    let canonical_legacy_home = legacy_home
        .strip_prefix(&original_data_root)
        .map(|relative| canonical_data_root.join(relative))
        .unwrap_or_else(|_| legacy_home.to_path_buf());
    let canonical_broker_home = broker_home
        .strip_prefix(&original_data_root)
        .map(|relative| canonical_data_root.join(relative))
        .unwrap_or_else(|_| broker_home.to_path_buf());
    let data_root_alias = original_data_root.as_path();
    let data_root = canonical_data_root.as_path();
    let legacy_home = canonical_legacy_home.as_path();
    let broker_home = canonical_broker_home.as_path();
    match tokio::fs::symlink_metadata(legacy_home).await {
        Ok(metadata) => {
            validate_codex_state_source_home(data_root, data_root_alias, legacy_home, &metadata)?
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "checking legacy Codex home {} before exposing broker state",
                    legacy_home.display()
                )
            });
        }
    }

    for child in CODEX_CONTINUITY_STATE_CHILDREN {
        expose_legacy_codex_state_child(
            data_root,
            data_root_alias,
            legacy_home,
            broker_home,
            child,
        )
        .await?;
    }

    Ok(())
}

async fn expose_legacy_codex_state_child(
    data_root: &Path,
    data_root_alias: &Path,
    legacy_home: &Path,
    broker_home: &Path,
    child: &ProviderStorageChild,
) -> Result<()> {
    let source = legacy_home.join(child.name);
    let dest = broker_home.join(child.name);
    let mut source_metadata = match tokio::fs::symlink_metadata(&source).await {
        Ok(metadata) => Some(metadata),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "checking legacy Codex state path {} before exposing broker state",
                    source.display()
                )
            });
        }
    };

    match tokio::fs::symlink_metadata(&dest).await {
        Ok(dest_metadata) if dest_metadata.file_type().is_symlink() => {
            let target = tokio::fs::read_link(&dest)
                .await
                .with_context(|| format!("reading broker Codex state link {}", dest.display()))?;
            if symlink_target_points_to_source(data_root, data_root_alias, &source, &dest, &target)
            {
                if source_metadata.is_none() {
                    source_metadata =
                        Some(ensure_shared_codex_state_child_exists(&source, child).await?);
                }
                if let Some(metadata) = source_metadata.as_ref() {
                    validate_codex_state_child_metadata(&source, metadata, child)?;
                }
                return Ok(());
            }
            repair_stale_broker_state_link(
                data_root,
                data_root_alias,
                &source,
                &dest,
                child,
                target,
            )
            .await?;
            return Ok(());
        }
        Ok(_) => {
            repair_preexisting_broker_state_child(&source, &dest, child).await?;
            return Ok(());
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(backup) = existing_codex_state_backup_path(&dest)? {
                repair_pending_broker_state_backup(&source, &dest, &backup, child).await?;
                return Ok(());
            }
            if source_metadata.is_none() {
                source_metadata =
                    Some(ensure_shared_codex_state_child_exists(&source, child).await?);
            }
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "checking broker Codex state path {} before exposing legacy state",
                    dest.display()
                )
            });
        }
    }

    if let Some(parent) = dest.parent() {
        ctx_fs::permissions::ensure_private_dir(parent).await?;
    }

    let source_for_link = source.clone();
    let dest_for_link = dest.clone();
    let source_is_dir = source_metadata
        .as_ref()
        .map(|metadata| validate_codex_state_child_metadata(&source, metadata, child))
        .transpose()?
        .unwrap_or(matches!(child.kind, ProviderStorageChildKind::Directory));
    let link_result = tokio::task::spawn_blocking(move || {
        create_codex_state_link(&source_for_link, &dest_for_link, source_is_dir)
    })
    .await
    .context("joining Codex broker state link task")?;

    match link_result {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "linking legacy Codex state {} into broker home at {}",
                source.display(),
                dest.display()
            )
        }),
    }
}

async fn acquire_codex_continuity_repair_lock(shared_home: &Path) -> Result<std::fs::File> {
    let shared_home = shared_home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let lock_root = shared_home
            .parent()
            .context("shared Codex continuity home has no parent")?;
        std::fs::create_dir_all(lock_root).with_context(|| {
            format!(
                "creating shared Codex provider root at {}",
                lock_root.display()
            )
        })?;
        let lock_path = lock_root.join(CODEX_CONTINUITY_REPAIR_LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| {
                format!(
                    "opening Codex continuity migration lock {}",
                    lock_path.display()
                )
            })?;
        file.lock_exclusive().with_context(|| {
            format!(
                "locking Codex continuity migration lock {}",
                lock_path.display()
            )
        })?;
        Ok(file)
    })
    .await
    .context("joining Codex continuity migration lock task")?
}

async fn merge_legacy_codex_state_home(
    data_root: &Path,
    data_root_alias: &Path,
    legacy_home: &Path,
    shared_home: &Path,
) -> Result<()> {
    match tokio::fs::symlink_metadata(legacy_home).await {
        Ok(metadata) => {
            validate_codex_state_source_home(data_root, data_root_alias, legacy_home, &metadata)?
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "checking legacy Codex home {} before merging shared state",
                    legacy_home.display()
                )
            });
        }
    }
    ctx_fs::permissions::ensure_private_dir(shared_home).await?;
    for child in CODEX_CONTINUITY_STATE_CHILDREN {
        let source = legacy_home.join(child.name);
        let dest = shared_home.join(child.name);
        let source_metadata = match tokio::fs::symlink_metadata(&source).await {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "checking legacy Codex state path {} before merging shared state",
                        source.display()
                    )
                });
            }
        };
        if source_metadata.file_type().is_symlink() {
            continue;
        }
        let source_is_dir = validate_codex_state_child_metadata(&source, &source_metadata, child)?;
        let source_for_merge = source.clone();
        let dest_for_merge = dest.clone();
        let child_for_merge = *child;
        tokio::task::spawn_blocking(move || {
            merge_codex_state_child(
                &source_for_merge,
                &dest_for_merge,
                &child_for_merge,
                source_is_dir,
            )
        })
        .await
        .context("joining Codex shared state merge task")?
        .with_context(|| {
            format!(
                "merging legacy Codex state {} into shared home at {}",
                source.display(),
                dest.display()
            )
        })?;
    }
    Ok(())
}

async fn ensure_shared_codex_state_child_exists(
    source: &Path,
    child: &ProviderStorageChild,
) -> Result<std::fs::Metadata> {
    if matches!(child.kind, ProviderStorageChildKind::Directory) {
        ctx_fs::permissions::ensure_private_dir(source).await?;
    } else {
        if let Some(parent) = source.parent() {
            ctx_fs::permissions::ensure_private_dir(parent).await?;
        }
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(source)
            .await
            .with_context(|| {
                format!("creating shared Codex continuity file {}", source.display())
            })?;
    }
    tokio::fs::symlink_metadata(source).await.with_context(|| {
        format!(
            "checking newly created shared Codex continuity state {}",
            source.display()
        )
    })
}

async fn repair_stale_broker_state_link(
    data_root: &Path,
    data_root_alias: &Path,
    source: &Path,
    dest: &Path,
    child: &ProviderStorageChild,
    target: PathBuf,
) -> Result<()> {
    let resolved_target = resolve_symlink_target(dest, &target)?;
    match tokio::fs::symlink_metadata(&resolved_target).await {
        Ok(_) => {
            ensure_stale_codex_state_link_target_is_safe(
                data_root,
                data_root_alias,
                &resolved_target,
                child,
            )?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            match tokio::fs::symlink_metadata(source).await {
                Ok(_) => {}
                Err(source_err) if source_err.kind() == std::io::ErrorKind::NotFound => {
                    ensure_shared_codex_state_child_exists(source, child).await?;
                }
                Err(source_err) => {
                    return Err(source_err).with_context(|| {
                        format!(
                            "checking shared Codex state {} before stale link repair",
                            source.display()
                        )
                    });
                }
            }
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "checking stale broker Codex link target {}",
                    resolved_target.display()
                )
            });
        }
    }
    let source_for_repair = source.to_path_buf();
    let dest_for_repair = dest.to_path_buf();
    let child_for_repair = *child;
    tokio::task::spawn_blocking(move || {
        if let Ok(target_metadata) = fs::metadata(&resolved_target) {
            merge_codex_state_child(
                &resolved_target,
                &source_for_repair,
                &child_for_repair,
                target_metadata.file_type().is_dir(),
            )?;
        }
        let source_is_dir = existing_codex_state_child_kind(&source_for_repair, &child_for_repair)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "missing canonical Codex continuity child {} after stale link repair",
                        source_for_repair.display()
                    ),
                )
            })?;
        let old_link_backup = next_codex_state_link_swap_path(&dest_for_repair)?;
        fs::rename(&dest_for_repair, &old_link_backup)?;
        match create_codex_state_link(&source_for_repair, &dest_for_repair, source_is_dir) {
            Ok(()) => {
                let _ = remove_codex_state_link(&old_link_backup, &child_for_repair);
                Ok(())
            }
            Err(create_err) => {
                let _ = remove_codex_state_link(&dest_for_repair, &child_for_repair);
                let _ = fs::rename(&old_link_backup, &dest_for_repair);
                Err(create_err)
            }
        }
    })
    .await
    .context("joining Codex stale broker state link repair task")?
    .with_context(|| {
        format!(
            "retargeting broker Codex state link {} to {}",
            dest.display(),
            source.display()
        )
    })?;
    Ok(())
}

fn ensure_stale_codex_state_link_target_is_safe(
    data_root: &Path,
    data_root_alias: &Path,
    target: &Path,
    child: &ProviderStorageChild,
) -> Result<()> {
    let target_link_metadata = fs::symlink_metadata(target).with_context(|| {
        format!(
            "checking stale Codex continuity link target {}",
            target.display()
        )
    })?;
    if target_link_metadata.file_type().is_symlink() {
        anyhow::bail!(
            "refusing to merge stale Codex continuity link target {} for child {}; target is a symlink",
            target.display(),
            child.name
        );
    }
    if path_has_symlink_component_below(data_root, data_root_alias, target)? {
        anyhow::bail!(
            "refusing to merge stale Codex continuity link target {} for child {}; target path contains a symlink",
            target.display(),
            child.name
        );
    }
    let target_metadata = fs::metadata(target).with_context(|| {
        format!(
            "checking stale Codex continuity link target metadata {}",
            target.display()
        )
    })?;
    validate_codex_state_child_metadata(target, &target_metadata, child)?;
    let canonical_target = fs::canonicalize(target).with_context(|| {
        format!(
            "canonicalizing stale Codex continuity link target {}",
            target.display()
        )
    })?;
    let allowed_targets = [
        codex_runtime_home(data_root).join(child.name),
        legacy_codex_runtime_home(data_root).join(child.name),
    ];
    for allowed_target in allowed_targets {
        let Ok(allowed_metadata) = fs::symlink_metadata(&allowed_target) else {
            continue;
        };
        if allowed_metadata.file_type().is_symlink() {
            continue;
        }
        if allowed_metadata.file_type().is_dir()
            != matches!(child.kind, ProviderStorageChildKind::Directory)
        {
            continue;
        }
        if let Ok(canonical_allowed) = fs::canonicalize(&allowed_target) {
            if canonical_allowed == canonical_target {
                return Ok(());
            }
        }
    }
    anyhow::bail!(
        "refusing to merge stale Codex continuity link target {} for child {}; target is not a declared Codex continuity child path",
        target.display(),
        child.name
    );
}

fn path_has_symlink_component(path: &Path) -> std::io::Result<bool> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(false)
}

fn path_has_symlink_component_below(
    root: &Path,
    alias_root: &Path,
    path: &Path,
) -> std::io::Result<bool> {
    let (base, relative) = if let Ok(value) = path.strip_prefix(root) {
        (root, value)
    } else if alias_root != root {
        match path.strip_prefix(alias_root) {
            Ok(value) => (alias_root, value),
            Err(_) => return path_has_symlink_component(path),
        }
    } else {
        return path_has_symlink_component(path);
    };
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(false)
}

fn validate_codex_state_source_home(
    data_root: &Path,
    data_root_alias: &Path,
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<()> {
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "Codex continuity source home {} must not be a symlink",
            path.display()
        );
    }
    if !metadata.file_type().is_dir() {
        anyhow::bail!(
            "Codex continuity source home {} must be a directory",
            path.display()
        );
    }
    if path_has_symlink_component_below(data_root, data_root_alias, path)? {
        anyhow::bail!(
            "Codex continuity source home {} must not contain a symlink component",
            path.display()
        );
    }
    Ok(())
}

fn validate_codex_runtime_home_path_before_lock(
    data_root: &Path,
    data_root_alias: &Path,
    home: &Path,
) -> Result<()> {
    if path_has_symlink_component_below(data_root, data_root_alias, home)? {
        anyhow::bail!(
            "Codex runtime home path {} must not contain a symlink component before lock acquisition",
            home.display()
        );
    }
    match fs::symlink_metadata(home) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                anyhow::bail!(
                    "Codex runtime home path {} must not be a symlink before lock acquisition",
                    home.display()
                );
            }
            if !metadata.file_type().is_dir() {
                anyhow::bail!(
                    "Codex runtime home path {} must be a directory before lock acquisition",
                    home.display()
                );
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "checking Codex runtime home path {} before lock acquisition",
                    home.display()
                )
            });
        }
    }
    Ok(())
}

async fn repair_preexisting_broker_state_child(
    source: &Path,
    dest: &Path,
    child: &ProviderStorageChild,
) -> Result<()> {
    let backup = next_codex_state_backup_path(dest)?;
    let source_for_repair = source.to_path_buf();
    let dest_for_repair = dest.to_path_buf();
    let backup_for_repair = backup.clone();
    let child_for_repair = *child;
    tokio::task::spawn_blocking(move || {
        fs::rename(&dest_for_repair, &backup_for_repair)?;
        let repair_result = (|| {
            let backup_is_dir =
                existing_codex_state_child_kind(&backup_for_repair, &child_for_repair)?
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!(
                                "Codex continuity backup {} disappeared",
                                backup_for_repair.display()
                            ),
                        )
                    })?;
            merge_codex_state_child(
                &backup_for_repair,
                &source_for_repair,
                &child_for_repair,
                backup_is_dir,
            )?;
            let source_is_dir =
                existing_codex_state_child_kind(&source_for_repair, &child_for_repair)?
                    .unwrap_or(backup_is_dir);
            create_codex_state_link(&source_for_repair, &dest_for_repair, source_is_dir)
        })();
        if repair_result.is_err() && !dest_for_repair.exists() {
            let _ = fs::rename(&backup_for_repair, &dest_for_repair);
        }
        repair_result
    })
    .await
    .context("joining Codex broker state repair task")?
    .with_context(|| {
        format!(
            "repairing preexisting broker Codex state {} with backup at {}",
            dest.display(),
            backup.display()
        )
    })?;
    Ok(())
}

async fn repair_pending_broker_state_backup(
    source: &Path,
    dest: &Path,
    backup: &Path,
    child: &ProviderStorageChild,
) -> Result<()> {
    let source_for_repair = source.to_path_buf();
    let dest_for_repair = dest.to_path_buf();
    let backup_for_repair = backup.to_path_buf();
    let child_for_repair = *child;
    tokio::task::spawn_blocking(move || {
        let backup_is_dir = existing_codex_state_child_kind(&backup_for_repair, &child_for_repair)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "Codex continuity backup {} disappeared",
                        backup_for_repair.display()
                    ),
                )
            })?;
        merge_codex_state_child(
            &backup_for_repair,
            &source_for_repair,
            &child_for_repair,
            backup_is_dir,
        )?;
        let source_is_dir = existing_codex_state_child_kind(&source_for_repair, &child_for_repair)?
            .unwrap_or(backup_is_dir);
        create_codex_state_link(&source_for_repair, &dest_for_repair, source_is_dir)
    })
    .await
    .context("joining Codex pending broker state backup repair task")?
    .with_context(|| {
        format!(
            "repairing pending broker Codex state backup {} into {}",
            backup.display(),
            dest.display()
        )
    })?;
    Ok(())
}

fn resolve_symlink_target(link_path: &Path, target: &Path) -> Result<PathBuf> {
    if target.is_absolute() {
        return Ok(target.to_path_buf());
    }
    let parent = link_path
        .parent()
        .context("broker Codex state link path has no parent")?;
    Ok(parent.join(target))
}

fn symlink_target_points_to_source(
    data_root: &Path,
    data_root_alias: &Path,
    source: &Path,
    link_path: &Path,
    target: &Path,
) -> bool {
    let Ok(resolved_target) = resolve_symlink_target(link_path, target) else {
        return false;
    };
    if std::fs::symlink_metadata(&resolved_target)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(true)
    {
        return false;
    }
    if std::fs::symlink_metadata(source)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(true)
    {
        return false;
    }
    if path_has_symlink_component_below(data_root, data_root_alias, &resolved_target)
        .unwrap_or(true)
        || path_has_symlink_component_below(data_root, data_root_alias, source).unwrap_or(true)
    {
        return false;
    }
    if target == source {
        return true;
    }
    normalize_path_lexically(&resolved_target) == normalize_path_lexically(source)
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn remove_codex_state_link(path: &Path, child: &ProviderStorageChild) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(file_err) if matches!(child.kind, ProviderStorageChildKind::Directory) => {
            fs::remove_dir(path).map_err(|_| file_err)
        }
        Err(err) => Err(err),
    }
}

fn next_codex_state_link_swap_path(dest: &Path) -> std::io::Result<PathBuf> {
    let parent = dest.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "broker Codex state link path {} has no parent",
                dest.display()
            ),
        )
    })?;
    let name = dest
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "broker Codex state link path {} has no UTF-8 file name",
                    dest.display()
                ),
            )
        })?;
    let pid = std::process::id();
    for index in 0..1000 {
        let candidate = parent.join(format!(".{name}.ctx-link-swap-{pid}-{index}"));
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "could not allocate Codex continuity link swap path for {}",
            dest.display()
        ),
    ))
}

fn existing_codex_state_backup_path(dest: &Path) -> Result<Option<PathBuf>> {
    let parent = dest
        .parent()
        .context("broker Codex state path has no parent")?;
    let backup_root = parent.join(".ctx-continuity-migration-backups");
    if !validate_existing_codex_state_backup_root(&backup_root)? {
        return Ok(None);
    }
    let name = dest
        .file_name()
        .and_then(|value| value.to_str())
        .context("broker Codex state path has no UTF-8 file name")?;
    let mut latest = None;
    for index in 0..1000 {
        let candidate = backup_root.join(format!("{name}.{index}"));
        if fs::symlink_metadata(&candidate).is_ok() {
            latest = Some(candidate);
        }
    }
    Ok(latest)
}

fn next_codex_state_backup_path(dest: &Path) -> Result<PathBuf> {
    let parent = dest
        .parent()
        .context("broker Codex state path has no parent")?;
    let backup_root = parent.join(".ctx-continuity-migration-backups");
    ensure_codex_state_backup_root(&backup_root).with_context(|| {
        format!(
            "creating Codex continuity migration backup dir {}",
            backup_root.display()
        )
    })?;
    let name = dest
        .file_name()
        .and_then(|value| value.to_str())
        .context("broker Codex state path has no UTF-8 file name")?;
    for index in 0..1000 {
        let candidate = backup_root.join(format!("{name}.{index}"));
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "checking Codex continuity backup path {}",
                        candidate.display()
                    )
                });
            }
        }
    }
    anyhow::bail!(
        "could not allocate Codex continuity backup path for {}",
        dest.display()
    );
}

fn validate_existing_codex_state_backup_root(backup_root: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(backup_root) {
        Ok(metadata) => {
            validate_codex_state_backup_root_metadata(backup_root, &metadata)?;
            Ok(true)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn ensure_codex_state_backup_root(backup_root: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(backup_root) {
        Ok(metadata) => validate_codex_state_backup_root_metadata(backup_root, &metadata)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(backup_root)?;
            let metadata = fs::symlink_metadata(backup_root)?;
            validate_codex_state_backup_root_metadata(backup_root, &metadata)?;
        }
        Err(err) => return Err(err),
    }
    Ok(())
}

fn validate_codex_state_backup_root_metadata(
    backup_root: &Path,
    metadata: &fs::Metadata,
) -> std::io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Codex continuity backup root {} must not be a symlink",
                backup_root.display()
            ),
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Codex continuity backup root {} must be a directory",
                backup_root.display()
            ),
        ));
    }
    Ok(())
}

fn existing_codex_state_child_kind(
    path: &Path,
    child: &ProviderStorageChild,
) -> std::io::Result<Option<bool>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Codex continuity child {} must not be a symlink",
                        path.display()
                    ),
                ));
            }
            let is_dir = metadata.file_type().is_dir();
            if is_dir != matches!(child.kind, ProviderStorageChildKind::Directory) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Codex continuity child {} kind does not match manifest",
                        path.display()
                    ),
                ));
            }
            if !is_dir && !metadata.file_type().is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Codex continuity child {} is not a regular file",
                        path.display()
                    ),
                ));
            }
            Ok(Some(is_dir))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn validate_codex_state_child_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
    child: &ProviderStorageChild,
) -> Result<bool> {
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "Codex continuity child {} must not be a symlink",
            path.display()
        );
    }
    let is_dir = metadata.file_type().is_dir();
    if is_dir != matches!(child.kind, ProviderStorageChildKind::Directory) {
        anyhow::bail!(
            "Codex continuity child {} kind does not match manifest",
            path.display()
        );
    }
    if !is_dir && !metadata.file_type().is_file() {
        anyhow::bail!(
            "Codex continuity child {} is not a regular file",
            path.display()
        );
    }
    Ok(is_dir)
}

fn merge_codex_state_child(
    source: &Path,
    dest: &Path,
    _child: &ProviderStorageChild,
    source_is_dir: bool,
) -> std::io::Result<()> {
    if source_is_dir {
        merge_codex_state_dir(source, dest)?;
        return Ok(());
    }
    if !dest.exists() {
        copy_codex_state_file_create_new(source, dest)?;
        return Ok(());
    }
    Ok(())
}

fn copy_codex_state_file_create_new(source: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("codex-state");
    for index in 0..1000 {
        let temp = parent.join(format!(
            ".{name}.ctx-continuity-copy-{}-{index}",
            std::process::id()
        ));
        let mut input = fs::File::open(source)?;
        let mut output = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        if let Err(err) = io::copy(&mut input, &mut output) {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        drop(output);
        match fs::hard_link(&temp, dest) {
            Ok(()) => {
                let _ = fs::remove_file(&temp);
                return Ok(());
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temp);
                return Ok(());
            }
            Err(err) => {
                let _ = fs::remove_file(&temp);
                return Err(err);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "could not allocate Codex continuity copy temp path for {}",
            dest.display()
        ),
    ))
}

fn merge_codex_state_dir(source: &Path, dest: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(dest) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Codex continuity directory {} must not be a symlink",
                        dest.display()
                    ),
                ));
            }
            if !metadata.file_type().is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Codex continuity directory {} is not a directory",
                        dest.display()
                    ),
                ));
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dest)?;
        }
        Err(err) => return Err(err),
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let entry_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let metadata = fs::symlink_metadata(&entry_path)?;
        if metadata.file_type().is_dir() {
            merge_codex_state_dir(&entry_path, &dest_path)?;
            continue;
        }
        if dest_path.exists() {
            continue;
        }
        if metadata.file_type().is_symlink() {
            continue;
        }
        if !metadata.file_type().is_file() {
            continue;
        }
        copy_codex_state_file_create_new(&entry_path, &dest_path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_codex_state_link(
    source: &Path,
    dest: &Path,
    _source_is_dir: bool,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, dest)
}

#[cfg(windows)]
fn create_codex_state_link(source: &Path, dest: &Path, source_is_dir: bool) -> std::io::Result<()> {
    if source_is_dir {
        std::os::windows::fs::symlink_dir(source, dest)
    } else {
        std::os::windows::fs::symlink_file(source, dest)
    }
}

#[cfg(not(any(unix, windows)))]
fn create_codex_state_link(source: &Path, dest: &Path, source_is_dir: bool) -> std::io::Result<()> {
    if source_is_dir {
        match std::fs::create_dir(dest) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(err) => Err(err),
        }
    } else {
        std::fs::hard_link(source, dest)
    }
}

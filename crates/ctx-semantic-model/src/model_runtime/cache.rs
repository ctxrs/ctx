use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(ctx_semantic_fastembed)]
use std::io::{Read, Seek};

#[cfg(any(target_os = "macos", test))]
use std::{
    process,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(any(target_os = "macos", test))]
use anyhow::bail;
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
#[cfg(ctx_semantic_fastembed)]
use uuid::Uuid;

#[cfg(ctx_semantic_fastembed)]
mod ort_variant;
#[cfg(ctx_semantic_fastembed)]
pub(crate) use ort_variant::{read_semantic_ort_model_file, semantic_ort_cache_snapshot};

#[cfg(all(test, ctx_semantic_fastembed))]
use super::semantic_model_acquisition_integrity_error;
#[cfg(ctx_semantic_fastembed)]
use super::{
    cache_paths::{
        semantic_model_cache_roots, SEMANTIC_HF_MODEL_CACHE_DIR, SEMANTIC_MANAGED_MODEL_CACHE_DIR,
    },
    SemanticCpuModelCacheMissing, SemanticCpuModelIntegrityError, SemanticModelFile,
    SemanticOrtModelVariant, SEMANTIC_MODEL_ID, SEMANTIC_MODEL_REVISION,
    SEMANTIC_REQUIRED_MODEL_FILES,
};

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn semantic_cpu_cache_snapshot(cache_dir: &Path) -> Result<PathBuf> {
    let mut repairable_error = None;
    for model_root in semantic_model_cache_roots(cache_dir) {
        let snapshot = model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
        match fs::metadata(&snapshot) {
            Ok(metadata) if metadata.is_dir() => match verify_semantic_cpu_snapshot(&snapshot) {
                Ok(()) => return Ok(snapshot),
                Err(error) if semantic_cpu_cache_repairable(&error) => {
                    repairable_error.get_or_insert(error);
                }
                Err(error) => return Err(error),
            },
            Ok(_) => {
                repairable_error.get_or_insert_with(|| {
                    SemanticCpuModelIntegrityError(format!(
                        "semantic CPU model snapshot {} is not a directory",
                        snapshot.display()
                    ))
                    .into()
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect semantic model cache {}", snapshot.display())
                });
            }
        }
    }
    Err(repairable_error.unwrap_or_else(|| {
        SemanticCpuModelCacheMissing(format!(
            "semantic model cache is incomplete at {}",
            cache_dir.display()
        ))
        .into()
    }))
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn semantic_cpu_cache_repairable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<SemanticCpuModelCacheMissing>()
        .is_some()
        || error
            .downcast_ref::<SemanticCpuModelIntegrityError>()
            .is_some()
}

#[cfg(ctx_semantic_fastembed)]
fn verify_semantic_cpu_snapshot(snapshot: &Path) -> Result<()> {
    for expected in SEMANTIC_REQUIRED_MODEL_FILES {
        verify_semantic_cpu_file(&snapshot.join(expected.path), *expected)?;
    }
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
fn verify_semantic_cpu_file(path: &Path, expected: SemanticModelFile) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SemanticCpuModelCacheMissing(format!(
                "semantic CPU model file {} is missing",
                path.display()
            ))
            .into());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect semantic CPU model file {}", path.display()));
        }
    };
    if !metadata.is_file() || metadata.len() != expected.size {
        return Err(SemanticCpuModelIntegrityError(format!(
            "semantic CPU model file {} has size {}, expected {}",
            path.display(),
            metadata.len(),
            expected.size
        ))
        .into());
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SemanticCpuModelCacheMissing(format!(
                "semantic CPU model file {} disappeared during verification",
                path.display()
            ))
            .into());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open semantic CPU model file {}", path.display()));
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("read semantic CPU model file {}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected.sha256 {
        return Err(SemanticCpuModelIntegrityError(format!(
            "semantic CPU model file {} has SHA-256 {actual}, expected {}",
            path.display(),
            expected.sha256
        ))
        .into());
    }
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn replace_cpu_model_cache_from_pinned_revision(cache_dir: &Path) -> Result<PathBuf> {
    use hf_hub::{api::sync::ApiBuilder, Repo, RepoType};

    let managed_root = cache_dir.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    fs::create_dir_all(&managed_root)
        .with_context(|| format!("create semantic model cache {}", managed_root.display()))?;
    let _lock = lock_semantic_model_acquisition(&managed_root)?;

    match semantic_cpu_cache_snapshot(cache_dir) {
        Ok(snapshot) => {
            let _ = cleanup_semantic_cpu_download_cache(&managed_root.join("download-cache"));
            return Ok(snapshot);
        }
        Err(error) if semantic_cpu_cache_repairable(&error) => {}
        Err(error) => return Err(error),
    }

    let download_cache = managed_root.join("download-cache");
    let model_root = managed_root.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let mut verified_staging_root = None;
    for attempt in 0..2 {
        if attempt > 0 {
            cleanup_semantic_cpu_download_cache(&download_cache)?;
        }
        prepare_semantic_cpu_download_cache(&download_cache)?;
        let api = ApiBuilder::new()
            .with_cache_dir(download_cache.clone())
            .with_progress(false)
            .build()
            .context("initialize pinned semantic model downloader")?;
        let repo = api.repo(Repo::with_revision(
            SEMANTIC_MODEL_ID.to_owned(),
            RepoType::Model,
            SEMANTIC_MODEL_REVISION.to_owned(),
        ));
        let staging_root = managed_root.join(format!(
            ".{SEMANTIC_HF_MODEL_CACHE_DIR}.staging-{}",
            Uuid::new_v4().simple()
        ));
        let staging_snapshot = staging_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
        let staged = (|| -> Result<()> {
            for expected in SEMANTIC_REQUIRED_MODEL_FILES {
                let downloaded = repo.download(expected.path).with_context(|| {
                    format!(
                        "download {SEMANTIC_MODEL_ID}@{SEMANTIC_MODEL_REVISION}/{}",
                        expected.path
                    )
                })?;
                let destination = staging_snapshot.join(expected.path);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "create semantic model staging directory {}",
                            parent.display()
                        )
                    })?;
                }
                stage_semantic_cpu_model_file(&downloaded, &download_cache, &destination)?;
            }
            verify_semantic_cpu_snapshot(&staging_snapshot).with_context(|| {
                format!(
                    "downloaded semantic CPU model failed verification in {}",
                    staging_snapshot.display()
                )
            })
        })();
        match staged {
            Ok(()) => {
                verified_staging_root = Some(staging_root);
                break;
            }
            Err(error) if attempt == 0 && semantic_cpu_cache_repairable(&error) => {
                let _ = fs::remove_dir_all(&staging_root);
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_root);
                return Err(error);
            }
        }
    }
    let staging_root = verified_staging_root.ok_or_else(|| {
        anyhow!("semantic CPU model download did not produce a verified snapshot")
    })?;

    if let Err(error) = publish_semantic_cpu_model_root(&staging_root, &model_root, &_lock) {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(error);
    }
    Ok(model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION))
}

#[cfg(ctx_semantic_fastembed)]
fn lock_semantic_model_acquisition(managed_root: &Path) -> Result<fs::File> {
    use fs2::FileExt;

    fs::create_dir_all(managed_root)
        .with_context(|| format!("create semantic model cache {}", managed_root.display()))?;
    let lock_path = managed_root.join("acquisition.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "open semantic model acquisition lock {}",
                lock_path.display()
            )
        })?;
    lock.lock_exclusive()
        .with_context(|| format!("lock semantic model acquisition {}", lock_path.display()))?;
    Ok(lock)
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition(
    cache_dir: &Path,
    daemon_owned: bool,
) {
    if !daemon_owned {
        return;
    }
    let managed_root = cache_dir.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let download_cache = managed_root.join("download-cache");
    if fs::symlink_metadata(&download_cache).is_err() {
        return;
    }
    let Ok(_lock) = lock_semantic_model_acquisition(&managed_root) else {
        return;
    };
    let _ = cleanup_semantic_cpu_download_cache(&download_cache);
}

#[cfg(ctx_semantic_fastembed)]
fn prepare_semantic_cpu_download_cache(download_cache: &Path) -> Result<()> {
    match fs::symlink_metadata(download_cache) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(anyhow!(
            "semantic model download cache {} has an unexpected filesystem shape",
            download_cache.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(download_cache).with_context(|| {
                format!(
                    "create semantic model download cache {}",
                    download_cache.display()
                )
            })?;
            let metadata = fs::symlink_metadata(download_cache).with_context(|| {
                format!(
                    "inspect created semantic model download cache {}",
                    download_cache.display()
                )
            })?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(anyhow!(
                    "created semantic model download cache {} has an unexpected filesystem shape",
                    download_cache.display()
                ))
            }
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect semantic model download cache {}",
                download_cache.display()
            )
        }),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn cleanup_semantic_cpu_download_cache(download_cache: &Path) -> Result<()> {
    match fs::symlink_metadata(download_cache) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(download_cache).with_context(|| {
                format!(
                    "remove ctx-managed semantic model download cache {}",
                    download_cache.display()
                )
            })
        }
        Ok(_) => Err(anyhow!(
            "refusing to remove semantic model download cache {} with an unexpected filesystem shape",
            download_cache.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect semantic model download cache {}",
                download_cache.display()
            )
        }),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn stage_semantic_cpu_model_file(
    downloaded: &Path,
    download_cache: &Path,
    destination: &Path,
) -> Result<()> {
    let (source, mut source_file) =
        open_verified_semantic_cpu_model_blob(downloaded, download_cache)?;
    stage_opened_semantic_cpu_model_blob(&source, &mut source_file, destination)
}

#[cfg(ctx_semantic_fastembed)]
fn open_verified_semantic_cpu_model_blob(
    downloaded: &Path,
    download_cache: &Path,
) -> Result<(PathBuf, fs::File)> {
    let canonical_cache = fs::canonicalize(download_cache).with_context(|| {
        format!(
            "resolve semantic model download cache {}",
            download_cache.display()
        )
    })?;
    let source = fs::canonicalize(downloaded).with_context(|| {
        format!(
            "resolve downloaded semantic model file {}",
            downloaded.display()
        )
    })?;
    if !source.starts_with(&canonical_cache) {
        return Err(anyhow!(
            "downloaded semantic model file {} resolves outside ctx-managed cache {}",
            downloaded.display(),
            download_cache.display()
        ));
    }
    let source_file = open_semantic_cpu_model_blob_nofollow(&source)?;
    if !source_file.metadata()?.is_file() {
        return Err(anyhow!(
            "downloaded semantic model blob {} is not a regular file",
            source.display()
        ));
    }
    let source_after_open = fs::canonicalize(&source)
        .with_context(|| format!("re-resolve semantic model blob {}", source.display()))?;
    if source_after_open != source || !source_after_open.starts_with(&canonical_cache) {
        return Err(anyhow!(
            "downloaded semantic model blob {} changed while opening",
            source.display()
        ));
    }
    #[cfg(unix)]
    if !semantic_cpu_model_path_matches_open_file(&source_file, &source)? {
        return Err(anyhow!(
            "downloaded semantic model blob {} changed while opening",
            source.display()
        ));
    }
    Ok((source, source_file))
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn stage_opened_semantic_cpu_model_blob(
    source: &Path,
    source_file: &mut fs::File,
    destination: &Path,
) -> Result<()> {
    stage_opened_semantic_cpu_model_blob_with_link(
        source,
        source_file,
        destination,
        |source, destination| fs::hard_link(source, destination),
    )
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn stage_opened_semantic_cpu_model_blob_with_link<F>(
    source: &Path,
    source_file: &mut fs::File,
    destination: &Path,
    hard_link: F,
) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    match hard_link(source, destination) {
        Ok(()) => {
            let staged_metadata = fs::symlink_metadata(destination).with_context(|| {
                format!(
                    "inspect hard-linked semantic model file {}",
                    destination.display()
                )
            })?;
            if !staged_metadata.is_file() || staged_metadata.file_type().is_symlink() {
                let _ = fs::remove_file(destination);
                return Err(anyhow!(
                    "hard-linked semantic model file {} is not a regular file",
                    destination.display()
                ));
            }
            let matches_source =
                semantic_cpu_model_path_matches_open_file(source_file, destination).with_context(
                    || {
                        format!(
                            "verify hard-linked semantic model file {}",
                            destination.display()
                        )
                    },
                );
            match matches_source {
                Ok(true) => {}
                Ok(false) => {
                    let _ = fs::remove_file(destination);
                    return Err(anyhow!(
                        "hard-linked semantic model file {} does not match the opened source",
                        destination.display()
                    ));
                }
                Err(error) => {
                    let _ = fs::remove_file(destination);
                    return Err(error);
                }
            }
            Ok(())
        }
        Err(link_error) => {
            copy_opened_semantic_cpu_model_blob(source_file, source, destination, Some(&link_error))
        }
    }
}

#[cfg(all(ctx_semantic_fastembed, windows))]
fn stage_opened_semantic_cpu_model_blob(
    source: &Path,
    source_file: &mut fs::File,
    destination: &Path,
) -> Result<()> {
    copy_opened_semantic_cpu_model_blob(source_file, source, destination, None)
}

#[cfg(all(ctx_semantic_fastembed, not(any(unix, windows))))]
fn stage_opened_semantic_cpu_model_blob(
    _source: &Path,
    _source_file: &mut fs::File,
    _destination: &Path,
) -> Result<()> {
    Err(anyhow!(
        "cannot safely stage semantic model blobs on this platform"
    ))
}

#[cfg(ctx_semantic_fastembed)]
fn copy_opened_semantic_cpu_model_blob(
    source_file: &mut fs::File,
    source: &Path,
    destination: &Path,
    link_error: Option<&std::io::Error>,
) -> Result<()> {
    source_file
        .rewind()
        .with_context(|| format!("rewind semantic model blob {}", source.display()))?;
    let mut destination_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| match link_error {
            Some(error) => format!(
                "create semantic model staging file {} after hard-link failure: {error}",
                destination.display()
            ),
            None => format!(
                "create semantic model staging file {}",
                destination.display()
            ),
        })?;
    std::io::copy(source_file, &mut destination_file).with_context(|| match link_error {
        Some(error) => format!(
            "copy semantic model blob {} to {} after hard-link failure: {error}",
            source.display(),
            destination.display()
        ),
        None => format!(
            "copy semantic model blob {} to {}",
            source.display(),
            destination.display()
        ),
    })?;
    Ok(())
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn open_semantic_cpu_model_blob_nofollow(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| {
            format!(
                "open semantic model blob without following symlinks {}",
                path.display()
            )
        })
}

#[cfg(all(ctx_semantic_fastembed, windows))]
fn open_semantic_cpu_model_blob_nofollow(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| {
            format!(
                "open semantic model blob without following reparse points {}",
                path.display()
            )
        })
}

#[cfg(all(ctx_semantic_fastembed, not(any(unix, windows))))]
fn open_semantic_cpu_model_blob_nofollow(path: &Path) -> Result<fs::File> {
    Err(anyhow!(
        "cannot safely open semantic model blob without following links on this platform: {}",
        path.display()
    ))
}

#[cfg(all(ctx_semantic_fastembed, unix))]
fn semantic_cpu_model_path_matches_open_file(opened: &fs::File, path: &Path) -> Result<bool> {
    let current = open_semantic_cpu_model_blob_nofollow(path)?;
    use std::os::unix::fs::MetadataExt;

    let opened = opened.metadata()?;
    let current = current.metadata()?;
    Ok(opened.dev() == current.dev() && opened.ino() == current.ino())
}

#[cfg(ctx_semantic_fastembed)]
fn publish_semantic_cpu_model_root(
    staging_root: &Path,
    model_root: &Path,
    _acquisition_lock: &fs::File,
) -> Result<()> {
    let managed_root = model_root
        .parent()
        .ok_or_else(|| anyhow!("semantic model root has no parent"))?;
    let backup_root = managed_root.join(format!(
        ".{SEMANTIC_HF_MODEL_CACHE_DIR}.backup-{}",
        Uuid::new_v4().simple()
    ));
    let had_previous = model_root.exists();
    if had_previous {
        fs::rename(model_root, &backup_root).with_context(|| {
            format!(
                "preserve previous semantic model cache {}",
                model_root.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(staging_root, model_root) {
        let restore = if had_previous {
            fs::rename(&backup_root, model_root).err()
        } else {
            None
        };
        return Err(anyhow!(match restore {
            Some(restore) => format!(
                "publish semantic model cache {}: {error}; restore previous cache: {restore}",
                model_root.display()
            ),
            None => format!(
                "publish semantic model cache {}: {error}",
                model_root.display()
            ),
        }));
    }
    if had_previous {
        // Publication is already committed. A cleanup failure must not turn a
        // valid model into a retry loop; a later acquisition may remove it.
        let _ = fs::remove_dir_all(&backup_root);
    }
    let _ = cleanup_semantic_cpu_download_cache(&managed_root.join("download-cache"));
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
#[allow(dead_code)] // Preserved compatibility helper for current cache tests.
pub(crate) fn read_semantic_model_file(snapshot: &Path, relative: &str) -> Result<Vec<u8>> {
    let path = snapshot.join(relative);
    fs::read(&path).with_context(|| format!("read semantic model file {}", path.display()))
}

#[cfg(any(target_os = "macos", test))]
mod compiled {
    use super::*;

    static COMPILE_STAGING_NONCE: AtomicU64 = AtomicU64::new(0);
    const MAX_COMPILER_IDENTITY_BYTES: usize = 512;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct CompileDestination {
        pub final_path: PathBuf,
        pub staging_path: PathBuf,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum AtomicCommit {
        Installed,
        AlreadyPresent,
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn create_private_dir_all(path: &Path) -> Result<()> {
        fs::create_dir_all(path)
            .with_context(|| format!("create model cache directory {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub(crate) fn prepare_compile_destination(
        compiled_cache_root: &Path,
        manifest_sha256: &str,
        model_role: &str,
        compiler_identity: &str,
    ) -> Result<CompileDestination> {
        validate_compiled_manifest_sha256(manifest_sha256)?;
        let role = match model_role {
            "document" | "query" => model_role,
            _ => bail!("compile model role must be document or query"),
        };
        validate_compiler_identity(compiler_identity)?;
        let compiler_hash = format!("{:x}", Sha256::digest(compiler_identity.as_bytes()));
        let parent = compiled_cache_root
            .join("coreml-compiled")
            .join("sha256")
            .join(manifest_sha256)
            .join(compiler_hash);
        create_compiled_directory_tree_nofollow(compiled_cache_root, &parent)?;
        let final_path = parent.join(format!("{role}.mlmodelc"));
        reject_compiled_symlink_if_present(&final_path)?;
        let staging_path = unique_compiled_sibling(&final_path)?;
        fs::create_dir(&staging_path).with_context(|| {
            format!(
                "create compile staging directory {}",
                staging_path.display()
            )
        })?;
        sync_compiled_parent(&staging_path)?;
        Ok(CompileDestination {
            final_path,
            staging_path,
        })
    }

    pub(crate) fn commit_compile_destination(
        destination: &CompileDestination,
    ) -> Result<AtomicCommit> {
        validate_compile_staging_pair(destination)?;
        require_compiled_real_directory(
            &destination.staging_path,
            "compiled model staging directory",
        )?;
        reject_compiled_symlinks_recursive(&destination.staging_path)?;
        sync_compiled_tree(&destination.staging_path)?;

        match fs::rename(&destination.staging_path, &destination.final_path) {
            Ok(()) => {
                sync_compiled_parent(&destination.final_path)?;
                Ok(AtomicCommit::Installed)
            }
            Err(rename_error) => {
                reject_compiled_symlink_if_present(&destination.final_path)?;
                if destination.final_path.is_dir() {
                    fs::remove_dir_all(&destination.staging_path).with_context(|| {
                        format!(
                            "remove redundant compile staging directory {}",
                            destination.staging_path.display()
                        )
                    })?;
                    Ok(AtomicCommit::AlreadyPresent)
                } else {
                    Err(rename_error).with_context(|| {
                        format!(
                            "atomically publish compiled model {}",
                            destination.final_path.display()
                        )
                    })
                }
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn discard_compile_destination(destination: &CompileDestination) -> Result<()> {
        validate_compile_staging_pair(destination)?;
        match fs::symlink_metadata(&destination.staging_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("refusing to remove symlinked compile staging path")
            }
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&destination.staging_path)
                .with_context(|| {
                    format!(
                        "remove compile staging directory {}",
                        destination.staging_path.display()
                    )
                }),
            Ok(_) => bail!("compile staging path is not a directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "inspect compile staging path {}",
                    destination.staging_path.display()
                )
            }),
        }
    }

    pub(crate) fn invalidate_compiled_model_cache(path: &Path) -> Result<()> {
        let file_name = path.file_name().and_then(|value| value.to_str());
        if !matches!(file_name, Some("document.mlmodelc" | "query.mlmodelc")) {
            bail!("refusing to invalidate unexpected compiled model cache path");
        }
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect compiled model cache {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("refusing to invalidate non-directory compiled model cache");
        }
        fs::remove_dir_all(path)
            .with_context(|| format!("invalidate compiled model cache {}", path.display()))
    }

    fn validate_compiled_manifest_sha256(value: &str) -> Result<()> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("manifest_sha256 must be 64 lowercase hexadecimal characters");
        }
        Ok(())
    }

    fn validate_compiler_identity(value: &str) -> Result<()> {
        if value.is_empty()
            || value.len() > MAX_COMPILER_IDENTITY_BYTES
            || value.chars().any(char::is_control)
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b':' | b'_' | b'-')
            })
        {
            bail!("compiler_identity contains unsupported characters");
        }
        Ok(())
    }

    fn create_compiled_directory_tree_nofollow(base: &Path, leaf: &Path) -> Result<()> {
        let relative = leaf
            .strip_prefix(base)
            .map_err(|_| anyhow!("cache path escaped its root"))?;
        require_compiled_real_directory(base, "compiled cache root")?;
        let mut current = base.to_path_buf();
        for component in relative.components() {
            let std::path::Component::Normal(component) = component else {
                bail!("compiled cache path contains invalid component");
            };
            current.push(component);
            match fs::create_dir(&current) {
                Ok(()) => sync_compiled_parent(&current)?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("create compiled cache directory {}", current.display())
                    });
                }
            }
            require_compiled_real_directory(&current, "compiled cache directory")?;
        }
        Ok(())
    }

    fn require_compiled_real_directory(path: &Path, description: &str) -> Result<()> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect {description} {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("{description} must be a real directory: {}", path.display());
        }
        Ok(())
    }

    fn reject_compiled_symlink_if_present(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("refusing symlink path {}", path.display())
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("inspect path {}", path.display())),
        }
    }

    fn reject_compiled_symlinks_recursive(root: &Path) -> Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "compiled model staging tree contains symlink {}",
                    path.display()
                );
            }
            if metadata.is_dir() {
                reject_compiled_symlinks_recursive(&path)?;
            } else if !metadata.is_file() {
                bail!(
                    "compiled model staging tree contains unsupported path {}",
                    path.display()
                );
            }
        }
        Ok(())
    }

    fn sync_compiled_tree(root: &Path) -> Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                sync_compiled_tree(&path)?;
            } else if metadata.is_file() {
                fs::File::open(&path)?.sync_all()?;
            }
        }
        sync_compiled_directory(root)
    }

    fn unique_compiled_sibling(destination: &Path) -> Result<PathBuf> {
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("destination has no parent"))?;
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("destination has no UTF-8 file name"))?;
        for _ in 0..128 {
            let nonce = COMPILE_STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(".{name}.compile.{}.{}.tmp", process::id(), nonce));
            if !candidate.exists() {
                reject_compiled_symlink_if_present(&candidate)?;
                return Ok(candidate);
            }
        }
        bail!("could not allocate unique staging path")
    }

    fn validate_compile_staging_pair(destination: &CompileDestination) -> Result<()> {
        if destination.final_path.parent() != destination.staging_path.parent() {
            bail!("compile staging and final paths must share a parent");
        }
        let final_name = destination
            .final_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("compile final path has no UTF-8 file name"))?;
        let staging_name = destination
            .staging_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("compile staging path has no UTF-8 file name"))?;
        if !staging_name.starts_with(&format!(".{final_name}.compile."))
            || !staging_name.ends_with(".tmp")
        {
            bail!("compile staging path is not associated with final path");
        }
        Ok(())
    }

    #[cfg(unix)]
    fn sync_compiled_directory(path: &Path) -> Result<()> {
        fs::File::open(path)
            .with_context(|| format!("open directory for sync {}", path.display()))?
            .sync_all()
            .with_context(|| format!("sync directory {}", path.display()))
    }

    #[cfg(not(unix))]
    fn sync_compiled_directory(_path: &Path) -> Result<()> {
        Ok(())
    }

    fn sync_compiled_parent(path: &Path) -> Result<()> {
        let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
        sync_compiled_directory(parent)
    }
}

#[cfg(target_os = "macos")]
pub(crate) use compiled::discard_compile_destination;
#[cfg(target_os = "macos")]
pub(crate) use compiled::{
    commit_compile_destination, create_private_dir_all, invalidate_compiled_model_cache,
    prepare_compile_destination,
};

#[cfg(all(test, ctx_semantic_fastembed))]
#[path = "cpu_model_cache_tests.rs"]
mod cpu_model_cache_tests;

#[cfg(test)]
#[path = "cache_tests.rs"]
mod compiled_cache_tests;

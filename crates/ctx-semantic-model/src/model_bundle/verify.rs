use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use super::{
    manifest::{
        sha256_bytes, validate_manifest, validate_relative_path, BundleFile, ModelBundleManifest,
        VerifiedModelBundle, MANIFEST_FILE, MAX_BUNDLE_DIRECTORIES, MAX_BUNDLE_FILES,
        MAX_FILE_BYTES, MAX_MANIFEST_BYTES,
    },
    secure_fs::{open_read_nofollow, require_real_directory, same_file_metadata},
};

pub(crate) fn verify_model_bundle(root: &Path) -> Result<VerifiedModelBundle> {
    require_real_directory(root, "model bundle root")?;
    let manifest_path = root.join(MANIFEST_FILE);
    let manifest_bytes = read_bounded_regular_file(&manifest_path, MAX_MANIFEST_BYTES)
        .with_context(|| format!("read model bundle manifest {}", manifest_path.display()))?;
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let manifest: ModelBundleManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parse model bundle manifest {}", manifest_path.display()))?;
    validate_manifest(&manifest)?;

    let actual_files = collect_bundle_files(root)?;
    let expected_files: BTreeSet<String> = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    if actual_files != expected_files {
        let missing: Vec<_> = expected_files.difference(&actual_files).cloned().collect();
        let unexpected: Vec<_> = actual_files.difference(&expected_files).cloned().collect();
        bail!(
            "model bundle file set does not match manifest (missing: {missing:?}, unexpected: {unexpected:?})"
        );
    }

    for entry in &manifest.files {
        let path = checked_join(root, &entry.path)?;
        verify_file(&path, entry)?;
    }

    Ok(VerifiedModelBundle {
        root: root.to_path_buf(),
        manifest,
        manifest_sha256,
    })
}

pub(super) fn collect_bundle_files(root: &Path) -> Result<BTreeSet<String>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeSet<String>,
        directory_count: &mut usize,
    ) -> Result<()> {
        *directory_count += 1;
        if *directory_count > MAX_BUNDLE_DIRECTORIES {
            bail!("model bundle contains too many directories");
        }
        let mut entries = fs::read_dir(directory)
            .with_context(|| format!("read model bundle directory {}", directory.display()))?
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        if entries.is_empty() && directory != root {
            bail!(
                "model bundle contains empty directory {}",
                directory.display()
            );
        }
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("inspect model bundle path {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!("model bundle contains symlink {}", path.display());
            }
            if metadata.is_dir() {
                visit(root, &path, files, directory_count)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| anyhow!("model bundle path escaped root"))?
                    .to_str()
                    .ok_or_else(|| anyhow!("model bundle path is not UTF-8"))?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                validate_relative_path(&relative)?;
                if relative != MANIFEST_FILE {
                    if files.len() >= MAX_BUNDLE_FILES {
                        bail!("model bundle contains too many files");
                    }
                    files.insert(relative);
                }
            } else {
                bail!("model bundle contains unsupported path {}", path.display());
            }
        }
        Ok(())
    }

    let mut files = BTreeSet::new();
    let mut directory_count = 0;
    visit(root, root, &mut files, &mut directory_count)?;
    Ok(files)
}

pub(super) fn verify_file(path: &Path, entry: &BundleFile) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect model bundle file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "model bundle path is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() != entry.size_bytes {
        bail!("model bundle file size mismatch for {}", entry.path);
    }
    let mut file = open_read_nofollow(path)?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("inspect opened model bundle file {}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() != entry.size_bytes {
        bail!("model bundle file changed while opening: {}", entry.path);
    }
    let actual = sha256_reader(&mut file, entry.size_bytes, path)?;
    if actual != entry.sha256 {
        bail!("model bundle SHA-256 mismatch for {}", entry.path);
    }
    let after = fs::symlink_metadata(path)
        .with_context(|| format!("reinspect model bundle file {}", path.display()))?;
    if after.file_type().is_symlink() || !same_file_metadata(&opened_metadata, &after) {
        bail!(
            "model bundle file changed during verification: {}",
            entry.path
        );
    }
    Ok(())
}

pub(super) fn sha256_reader(file: &mut File, expected_size: u64, path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut read_bytes = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("hash model bundle file {}", path.display()))?;
        if count == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("model bundle file size overflow"))?;
        if read_bytes > expected_size || read_bytes > MAX_FILE_BYTES {
            bail!("model bundle file grew while hashing: {}", path.display());
        }
        hasher.update(&buffer[..count]);
    }
    if read_bytes != expected_size {
        bail!(
            "model bundle file changed size while hashing: {}",
            path.display()
        );
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn read_bounded_regular_file(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("path is not a regular non-symlink file: {}", path.display());
    }
    if metadata.len() > maximum {
        bail!("file exceeds size limit: {}", path.display());
    }
    let mut file = open_read_nofollow(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        bail!("file exceeds size limit: {}", path.display());
    }
    Ok(bytes)
}

pub(super) fn checked_join(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_path(relative)?;
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            bail!("invalid model bundle path {relative}");
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect model bundle path {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("model bundle path traverses symlink {}", path.display());
        }
    }
    Ok(path)
}

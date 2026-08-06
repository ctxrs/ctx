use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    path::{Component, Path},
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::{
    acquisition_error, archive_integrity, ModelAcquisitionErrorKind, MAX_ARCHIVE_BYTES,
    MAX_EXPANDED_ARCHIVE_BYTES,
};
use crate::{
    model_bundle::{
        create_signed_bundle_staging_file, validate_relative_path, MAX_BUNDLE_BYTES,
        MAX_BUNDLE_DIRECTORIES, MAX_BUNDLE_FILES, MAX_FILE_BYTES,
    },
    model_contract::CoreMlBundleContract,
};

pub(super) fn verify_archive_hash(path: &Path, expected: &str) -> Result<()> {
    let mut file = fs::File::open(path).context("open downloaded Core ML archive")?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).context("hash Core ML archive")?;
        if read == 0 {
            break;
        }
        count = count.saturating_add(read as u64);
        if count > MAX_ARCHIVE_BYTES {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Integrity,
                "downloaded archive exceeds compressed size limit",
            ));
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "downloaded archive SHA-256 does not match the compiled descriptor",
        ));
    }
    Ok(())
}

pub(super) fn extract_archive(
    archive_path: &Path,
    destination: &Path,
    descriptor: &CoreMlBundleContract<'_>,
) -> Result<()> {
    let file = fs::File::open(archive_path).context("open verified Core ML archive")?;
    let decoder = xz2::read::XzDecoder::new(file);
    let bounded = ExpandedReader::new(decoder, MAX_EXPANDED_ARCHIVE_BYTES);
    let mut archive = tar::Archive::new(bounded);
    let expected_root = descriptor
        .artifact_name
        .strip_suffix(".tar.xz")
        .ok_or_else(|| {
            acquisition_error(ModelAcquisitionErrorKind::Integrity, "invalid archive name")
        })?;
    let mut seen = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut files = 0_usize;
    let mut payload_bytes = 0_u64;
    let entries = archive.entries().map_err(archive_error)?;
    for item in entries {
        let mut entry = item.map_err(archive_error)?;
        let raw_path = entry.path().map_err(archive_error)?;
        let relative = archive_relative_path(&raw_path, expected_root)?;
        if relative.is_empty() {
            if !entry.header().entry_type().is_dir() || !seen.insert(String::new()) {
                return Err(archive_integrity(
                    "archive root entry is invalid or duplicated",
                ));
            }
            continue;
        }
        validate_relative_path(&relative).map_err(|error| archive_integrity(error.to_string()))?;
        if !seen.insert(relative.clone()) {
            return Err(archive_integrity("archive contains duplicate paths"));
        }
        let target = destination.join(&relative);
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            register_directory(destination, &target, &relative, &mut directories)?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(archive_integrity(
                "archive contains a link, device, sparse, or unknown entry type",
            ));
        }
        files += 1;
        if files > MAX_BUNDLE_FILES {
            return Err(archive_integrity("archive contains too many files"));
        }
        let size = entry.header().size().map_err(archive_error)?;
        if size > MAX_FILE_BYTES {
            return Err(archive_integrity(
                "archive member exceeds per-file size limit",
            ));
        }
        payload_bytes = payload_bytes
            .checked_add(size)
            .ok_or_else(|| archive_integrity("archive payload size overflow"))?;
        if payload_bytes > MAX_BUNDLE_BYTES {
            return Err(archive_integrity(
                "archive exceeds expanded payload size limit",
            ));
        }
        create_parent_directories(destination, &target, &relative, &mut directories)?;
        let mut output = create_signed_bundle_staging_file(&target)
            .map_err(|error| archive_integrity(format!("create extracted file: {error}")))?;
        let copied = copy_exact_limited(&mut entry, &mut output, size)?;
        if copied != size {
            return Err(archive_integrity(
                "archive member size does not match its header",
            ));
        }
        output
            .sync_all()
            .map_err(|error| archive_integrity(format!("sync extracted file: {error}")))?;
    }
    if files == 0 {
        return Err(archive_integrity("archive contains no payload files"));
    }
    Ok(())
}

pub(super) fn archive_relative_path(path: &Path, expected_root: &str) -> Result<String> {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        return Err(archive_integrity(
            "archive path has no normal root component",
        ));
    };
    if root.to_str() != Some(expected_root) {
        return Err(archive_integrity(
            "archive path has an unexpected root directory",
        ));
    }
    let mut parts = Vec::new();
    for component in components {
        let Component::Normal(component) = component else {
            return Err(archive_integrity("archive path contains traversal"));
        };
        parts.push(
            component
                .to_str()
                .ok_or_else(|| archive_integrity("archive path is not UTF-8"))?,
        );
    }
    Ok(parts.join("/"))
}

pub(super) fn register_directory(
    destination: &Path,
    target: &Path,
    relative: &str,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    create_parent_directories(destination, target, relative, directories)?;
    if directories.insert(relative.to_owned()) {
        if directories.len() > MAX_BUNDLE_DIRECTORIES {
            return Err(archive_integrity("archive contains too many directories"));
        }
        match fs::create_dir(target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && target.is_dir() => {}
            Err(error) => {
                return Err(archive_integrity(format!(
                    "create extracted directory: {error}"
                )))
            }
        }
    }
    Ok(())
}

pub(super) fn create_parent_directories(
    destination: &Path,
    target: &Path,
    relative: &str,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    let Some(parent) = target.parent() else {
        return Err(archive_integrity("archive target has no parent"));
    };
    if parent == destination {
        return Ok(());
    }
    let mut current = destination.to_path_buf();
    let mut name = String::new();
    let parent_relative = Path::new(relative)
        .parent()
        .ok_or_else(|| archive_integrity("archive path parent is invalid"))?;
    for component in parent_relative.components() {
        let Component::Normal(component) = component else {
            return Err(archive_integrity("archive parent path contains traversal"));
        };
        let component = component
            .to_str()
            .ok_or_else(|| archive_integrity("archive parent path is not UTF-8"))?;
        if !name.is_empty() {
            name.push('/');
        }
        name.push_str(component);
        current.push(component);
        if directories.insert(name.clone()) {
            if directories.len() > MAX_BUNDLE_DIRECTORIES {
                return Err(archive_integrity("archive contains too many directories"));
            }
            fs::create_dir(&current).map_err(|error| {
                archive_integrity(format!("create extracted parent directory: {error}"))
            })?;
        } else if !current.is_dir() {
            return Err(archive_integrity(
                "archive path collides with a non-directory entry",
            ));
        }
    }
    Ok(())
}

pub(super) fn copy_exact_limited(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected: u64,
) -> Result<u64> {
    let mut limited = reader.take(expected.saturating_add(1));
    let copied = io::copy(&mut limited, writer)
        .map_err(|error| archive_integrity(format!("extract archive member: {error}")))?;
    if copied > expected {
        return Err(archive_integrity(
            "archive member exceeds its declared size",
        ));
    }
    Ok(copied)
}

pub(super) fn archive_error(error: io::Error) -> anyhow::Error {
    archive_integrity(format!("read compressed archive: {error}"))
}

pub(super) struct ExpandedReader<R> {
    inner: R,
    read: u64,
    maximum: u64,
}

impl<R> ExpandedReader<R> {
    fn new(inner: R, maximum: u64) -> Self {
        Self {
            inner,
            read: 0,
            maximum,
        }
    }
}

impl<R: Read> Read for ExpandedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.read = self.read.saturating_add(count as u64);
        if self.read > self.maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expanded archive exceeds size limit",
            ));
        }
        Ok(count)
    }
}

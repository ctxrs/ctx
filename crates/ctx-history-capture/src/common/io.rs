use std::{
    fs,
    io::BufRead,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    CaptureError, ProviderImportSummary, ProviderJsonlInventoryLimit, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod root_handle;
#[allow(
    unused_imports,
    reason = "provider adapters migrate to these capability types in follow-up slices"
)]
pub(crate) use root_handle::{
    open_provider_source_file, open_provider_source_path, OpenedProviderSourceFile,
    OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot,
};

/// Maximum directories admitted by one provider JSONL inventory.
///
/// Codex's ordinary layout uses only a few year/month/day levels. This leaves
/// ample room for unusually partitioned archives while bounding the stack.
pub const PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES: usize = 32_768;
/// Maximum child depth below the requested provider root.
pub const PROVIDER_JSONL_INVENTORY_MAX_DEPTH: usize = 64;
/// Maximum regular `.jsonl` paths retained by one provider inventory.
///
/// This preserves the prior Codex ceiling and is more than three times the file
/// count of the qualified 75 GiB physical corpus.
pub const PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS: usize = 131_072;
/// Maximum filesystem entries inspected, including non-JSONL entries.
pub const PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES: usize = 262_144;
/// Maximum encoded path length retained during provider inventory.
pub const PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderJsonlInventoryLimits {
    /// Maximum directories, including the requested root directory.
    pub max_directories: usize,
    /// Maximum child depth below the requested root, whose depth is zero.
    pub max_depth: usize,
    /// Maximum regular `.jsonl` files returned to the caller.
    pub max_eligible_paths: usize,
    /// Maximum inspected entries, including the requested root and junk files.
    pub max_metadata_entries: usize,
}

impl Default for ProviderJsonlInventoryLimits {
    fn default() -> Self {
        Self {
            max_directories: PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
            max_depth: PROVIDER_JSONL_INVENTORY_MAX_DEPTH,
            max_eligible_paths: PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
            max_metadata_entries: PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderJsonlInventory {
    paths: Vec<PathBuf>,
    directories: usize,
    metadata_entries: usize,
}

impl ProviderJsonlInventory {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn into_paths(self) -> Vec<PathBuf> {
        self.paths
    }

    pub fn directories(&self) -> usize {
        self.directories
    }

    pub fn metadata_entries(&self) -> usize {
        self.metadata_entries
    }
}

#[derive(Debug)]
struct PendingJsonlDirectory {
    relative_path: PathBuf,
    depth: usize,
}

#[derive(Debug)]
struct ProviderJsonlInventoryState {
    limits: ProviderJsonlInventoryLimits,
    paths: Vec<PathBuf>,
    directories: usize,
    metadata_entries: usize,
}

impl ProviderJsonlInventoryState {
    fn new(limits: ProviderJsonlInventoryLimits) -> Self {
        Self {
            limits,
            paths: Vec::new(),
            directories: 0,
            metadata_entries: 0,
        }
    }

    fn admit_metadata_entry(&mut self) -> Result<()> {
        self.metadata_entries = admit_inventory_unit(
            ProviderJsonlInventoryLimit::MetadataEntries,
            self.metadata_entries,
            self.limits.max_metadata_entries,
        )?;
        Ok(())
    }

    fn admit_directory(&mut self, depth: usize) -> Result<()> {
        if depth > self.limits.max_depth {
            return Err(inventory_limit_error(
                ProviderJsonlInventoryLimit::Depth,
                self.limits.max_depth,
            ));
        }
        self.directories = admit_inventory_unit(
            ProviderJsonlInventoryLimit::Directories,
            self.directories,
            self.limits.max_directories,
        )?;
        Ok(())
    }

    fn admit_eligible_path(&mut self, path: PathBuf) -> Result<()> {
        admit_inventory_unit(
            ProviderJsonlInventoryLimit::EligiblePaths,
            self.paths.len(),
            self.limits.max_eligible_paths,
        )?;
        self.paths.push(path);
        Ok(())
    }

    fn finish(mut self) -> ProviderJsonlInventory {
        self.paths.sort();
        ProviderJsonlInventory {
            paths: self.paths,
            directories: self.directories,
            metadata_entries: self.metadata_entries,
        }
    }
}

fn admit_inventory_unit(
    limit: ProviderJsonlInventoryLimit,
    current: usize,
    maximum: usize,
) -> Result<usize> {
    let observed = current.saturating_add(1);
    if observed > maximum {
        return Err(CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit,
            maximum,
            observed,
        });
    }
    Ok(observed)
}

fn inventory_limit_error(limit: ProviderJsonlInventoryLimit, maximum: usize) -> CaptureError {
    CaptureError::ProviderJsonlInventoryLimitExceeded {
        limit,
        maximum,
        observed: maximum.saturating_add(1),
    }
}

fn path_is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
}

fn ensure_inventory_path_bound(path: &Path) -> Result<()> {
    if path.as_os_str().as_encoded_bytes().len() > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "provider source path exceeds {PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES} encoded bytes"
        )));
    }
    Ok(())
}

/// Inventories one provider JSONL file or tree without recursive traversal.
///
/// The result is lexically sorted and contains only admitted regular `.jsonl`
/// files. Every encountered child, including non-JSONL and non-regular
/// entries, consumes the metadata-entry budget. Links are never followed.
pub fn inventory_provider_jsonl_paths(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    inventory_provider_paths(root, limits, path_is_jsonl)
}

/// Inventories every regular file under one provider source without recursive
/// traversal. This is used for format-neutral source accounting; provider
/// readers should use the narrower JSONL inventory above when appropriate.
pub fn inventory_provider_regular_paths(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    inventory_provider_paths(root, limits, |_| true)
}

fn inventory_provider_paths(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
    is_eligible: impl Fn(&Path) -> bool,
) -> Result<ProviderJsonlInventory> {
    let mut state = ProviderJsonlInventoryState::new(limits);
    state.admit_metadata_entry()?;
    ensure_inventory_path_bound(root)?;
    let opened_root = open_provider_source_path(root)?;
    if let OpenedProviderSourcePath::File(file) = opened_root {
        if is_eligible(root) {
            state.admit_eligible_path(root.to_path_buf())?;
        }
        file.revalidate()?;
        return Ok(state.finish());
    }
    let OpenedProviderSourcePath::Directory(root_directory) = opened_root else {
        return Err(CaptureError::SystemInvariant(
            "provider source root classification is incomplete",
        ));
    };
    let authority = root_directory.authority_root();

    state.admit_directory(0)?;
    let mut stack = vec![PendingJsonlDirectory {
        relative_path: PathBuf::new(),
        depth: 0,
    }];
    while let Some(pending) = stack.pop() {
        let directory = authority.open_directory(&pending.relative_path)?;
        let maximum_entries = state.limits.max_metadata_entries.saturating_add(1);
        let children = directory.entries(maximum_entries)?;

        let child_depth = pending.depth.saturating_add(1);
        let mut child_directories = Vec::new();
        for name in children {
            state.admit_metadata_entry()?;
            let relative_path = pending.relative_path.join(&name);
            let path = root.join(&relative_path);
            ensure_inventory_path_bound(&path)?;
            match directory.open_child(&name)? {
                OpenedProviderSourcePath::Directory(_) => {
                    state.admit_directory(child_depth)?;
                    child_directories.push(PendingJsonlDirectory {
                        relative_path,
                        depth: child_depth,
                    });
                }
                OpenedProviderSourcePath::File(_) if is_eligible(&path) => {
                    state.admit_eligible_path(path)?;
                }
                OpenedProviderSourcePath::File(_) => {}
            }
        }
        directory.revalidate()?;
        for child in child_directories.into_iter().rev() {
            stack.push(child);
        }
    }
    authority.revalidate()?;
    Ok(state.finish())
}

#[cfg(test)]
pub(crate) fn collect_jsonl_paths_bounded(
    root: &Path,
    paths: &mut Vec<PathBuf>,
    max_paths: usize,
) -> Result<()> {
    let inventory = inventory_provider_jsonl_paths(
        root,
        ProviderJsonlInventoryLimits {
            max_eligible_paths: max_paths,
            ..ProviderJsonlInventoryLimits::default()
        },
    )?;
    paths.extend(inventory.into_paths());
    Ok(())
}

/// Returns the length of an ordinary provider file without following links or
/// opening its contents.
///
/// Volatile accounting-only files such as SQLite `-shm` sidecars must
/// contribute to source totals without becoming revision authority or
/// introducing read-time mutation failures.
pub fn provider_regular_file_len(path: &Path) -> Result<u64> {
    let file = open_provider_source_file(path)?;
    let length = file.len();
    file.revalidate()?;
    Ok(length)
}

pub(crate) fn ensure_regular_provider_transcript_file(path: &Path) -> Result<()> {
    provider_regular_file_len(path)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn ensure_supported_windows_provider_path_prefix(path: &Path) -> Result<()> {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Ok(());
    };
    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason:
                "network, UNC, device, and other unsupported Windows provider roots are rejected",
        });
    }
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "drive-relative provider transcript paths are rejected",
        });
    }
    Ok(())
}

pub(crate) fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    ensure_supported_windows_provider_path_prefix(path)?;

    let parent_count = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for component in path.components().take(parent_count) {
        current.push(component.as_os_str());
        #[cfg(target_os = "windows")]
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        if current.as_os_str().is_empty() {
            continue;
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if provider_metadata_is_link_like(&metadata) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript path components are rejected",
            });
        }
    }
    Ok(())
}

pub(crate) fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

pub(crate) fn provider_metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(target_os = "windows"))]
    false
}

pub(crate) fn read_text_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
    let file = open_provider_source_file(path)?;
    let bytes = file.read_all_bounded(max_bytes).map_err(|error| {
        if matches!(error, CaptureError::InvalidPayload(_)) {
            CaptureError::InvalidPayload(format!("{label} exceeds max bytes ({max_bytes})"))
        } else {
            error
        }
    })?;
    String::from_utf8(bytes)
        .map_err(|err| CaptureError::InvalidPayload(format!("{label} is not valid UTF-8: {err}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderJsonlLineRead {
    Eof,
    Line { bytes: usize },
    Oversized { bytes: usize },
}

pub(crate) fn read_provider_jsonl_record_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    line_number: &mut usize,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    loop {
        match read_provider_jsonl_line_or_skip_oversized(reader, buffer)? {
            ProviderJsonlLineRead::Eof => return Ok(false),
            ProviderJsonlLineRead::Line { .. } => {
                *line_number = line_number.saturating_add(1);
                return Ok(true);
            }
            ProviderJsonlLineRead::Oversized { .. } => {
                *line_number = line_number.saturating_add(1);
                summary.skipped += 1;
                summary.skipped_events += 1;
            }
        }
    }
}

pub(crate) fn read_provider_jsonl_line_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<ProviderJsonlLineRead> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total > 0 {
                ProviderJsonlLineRead::Line { bytes: total }
            } else {
                ProviderJsonlLineRead::Eof
            });
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
                reader.consume(bytes_to_consume);
                buffer.clear();
                return Ok(ProviderJsonlLineRead::Oversized {
                    bytes: total.saturating_add(bytes_to_consume),
                });
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            return Ok(ProviderJsonlLineRead::Line {
                bytes: total.saturating_add(bytes_to_consume),
            });
        }

        let bytes_to_consume = available.len();
        if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
            reader.consume(bytes_to_consume);
            let discarded = discard_provider_jsonl_line(reader)?;
            buffer.clear();
            return Ok(ProviderJsonlLineRead::Oversized {
                bytes: total
                    .saturating_add(bytes_to_consume)
                    .saturating_add(discarded),
            });
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
        total = total.saturating_add(bytes_to_consume);
    }
}

pub(crate) fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<usize> {
    let mut discarded = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(discarded);
        }
        let bytes_to_consume = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let found_newline = available
            .get(bytes_to_consume.saturating_sub(1))
            .is_some_and(|byte| *byte == b'\n');
        reader.consume(bytes_to_consume);
        discarded = discarded.saturating_add(bytes_to_consume);
        if found_newline {
            return Ok(discarded);
        }
    }
}

pub(crate) fn read_json_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<Value> {
    let text = read_text_file_limited(path, max_bytes, label)?;
    serde_json::from_str(&text).map_err(CaptureError::from)
}

#[cfg(test)]
#[path = "io_tests.rs"]
mod tests;

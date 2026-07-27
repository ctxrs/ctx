use std::{
    fs::{self, Metadata},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use sha2::{Digest, Sha256};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, provider_metadata_is_link_like,
};
use crate::{CaptureError, Result};

use super::dto::{
    GeminiDiscovery, GeminiFileObservation, GeminiTranscriptLayout, GeminiTranscriptSource,
};

pub(super) const MAX_GEMINI_DISCOVERY_DEPTH: usize = 64;
pub(super) const MAX_GEMINI_DISCOVERY_ENTRIES: usize = 100_000;
pub(super) const MAX_GEMINI_DISCOVERY_PATH_BYTES: usize = 64 * 1024 * 1024;
const INVENTORY_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-inventory-v1\0";

#[derive(Debug)]
pub(super) struct DiscoveryBudget {
    entries: usize,
    path_bytes: usize,
    max_entries: usize,
    max_path_bytes: usize,
}

impl Default for DiscoveryBudget {
    fn default() -> Self {
        Self {
            entries: 0,
            path_bytes: 0,
            max_entries: MAX_GEMINI_DISCOVERY_ENTRIES,
            max_path_bytes: MAX_GEMINI_DISCOVERY_PATH_BYTES,
        }
    }
}

impl DiscoveryBudget {
    #[cfg(test)]
    pub(super) fn with_limits(max_entries: usize, max_path_bytes: usize) -> Self {
        Self {
            entries: 0,
            path_bytes: 0,
            max_entries,
            max_path_bytes,
        }
    }

    pub(super) fn observe(&mut self, path: &Path) -> Result<()> {
        let next_entries = self.entries.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Gemini transcript discovery entry count overflowed".to_owned(),
            )
        })?;
        if next_entries > self.max_entries {
            return Err(CaptureError::InvalidPayload(format!(
                "Gemini transcript discovery exceeds {} entries",
                self.max_entries
            )));
        }
        let next_path_bytes = self
            .path_bytes
            .checked_add(path.as_os_str().as_encoded_bytes().len())
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Gemini transcript discovery path-byte count overflowed".to_owned(),
                )
            })?;
        if next_path_bytes > self.max_path_bytes {
            return Err(CaptureError::InvalidPayload(format!(
                "Gemini transcript discovery exceeds {} path bytes",
                self.max_path_bytes
            )));
        }
        self.entries = next_entries;
        self.path_bytes = next_path_bytes;
        Ok(())
    }
}

impl GeminiFileObservation {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if provider_metadata_is_link_like(&metadata) || !metadata.file_type().is_file() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Gemini transcript paths must be ordinary regular files",
            });
        }
        ensure_provider_path_parents_are_not_symlinks(path)?;
        Self::from_metadata(&metadata)
    }

    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }
}

pub(crate) fn discover_gemini_transcripts(root: &Path) -> Result<GeminiDiscovery> {
    discover_gemini_transcripts_with_budget(root, DiscoveryBudget::default())
}

#[cfg(test)]
pub(super) fn discover_gemini_transcripts_with_limits(
    root: &Path,
    max_entries: usize,
    max_path_bytes: usize,
) -> Result<GeminiDiscovery> {
    discover_gemini_transcripts_with_budget(
        root,
        DiscoveryBudget::with_limits(max_entries, max_path_bytes),
    )
}

fn discover_gemini_transcripts_with_budget(
    root: &Path,
    mut budget: DiscoveryBudget,
) -> Result<GeminiDiscovery> {
    let metadata = fs::symlink_metadata(root)?;
    if provider_metadata_is_link_like(&metadata) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked Gemini transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    let canonical_root = fs::canonicalize(root)?;
    let layout_root = gemini_layout_root(&canonical_root, &metadata);
    let mut paths = Vec::new();
    let scan_root = gemini_scan_root(&canonical_root, &metadata)?;
    if let Some(scan_root) = scan_root {
        budget.observe(&scan_root)?;
        collect_candidates(&scan_root, &mut paths, &mut budget, 0)?;
    }
    paths.sort_unstable();

    let mut transcripts = Vec::with_capacity(paths.len());
    for path in paths {
        let Some(layout) =
            gemini_transcript_layout(&layout_root, &path, metadata.file_type().is_file())?
        else {
            continue;
        };
        let relative_path = if metadata.file_type().is_file() {
            path.file_name().map(PathBuf::from).unwrap_or_default()
        } else {
            path.strip_prefix(&canonical_root)
                .unwrap_or(&path)
                .to_path_buf()
        };
        transcripts.push(GeminiTranscriptSource {
            observation: GeminiFileObservation::read(&path)?,
            path,
            relative_path,
            layout,
        });
    }

    let inventory_sha256 = inventory_digest(&transcripts);
    Ok(GeminiDiscovery {
        root: canonical_root,
        transcripts,
        completed_inventory: true,
        inventory_sha256,
    })
}

fn gemini_scan_root(root: &Path, metadata: &Metadata) -> Result<Option<PathBuf>> {
    if metadata.file_type().is_file() {
        return Ok(Some(root.to_path_buf()));
    }
    let tmp = root.join("tmp");
    match fs::symlink_metadata(&tmp) {
        Ok(metadata) if provider_metadata_is_link_like(&metadata) => {
            Err(CaptureError::InvalidProviderTranscriptPath {
                path: tmp,
                reason: "linked Gemini transcript path components are rejected",
            })
        }
        Ok(metadata) if metadata.file_type().is_dir() => Ok(Some(tmp)),
        Ok(_) if root.file_name().is_some_and(|name| name == ".gemini") => Ok(None),
        Ok(_) => Ok(Some(root.to_path_buf())),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && root.file_name().is_some_and(|name| name == ".gemini") =>
        {
            Ok(None)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some(root.to_path_buf())),
        Err(error) => Err(error.into()),
    }
}

fn collect_candidates(
    path: &Path,
    paths: &mut Vec<PathBuf>,
    budget: &mut DiscoveryBudget,
    depth: usize,
) -> Result<()> {
    if depth > MAX_GEMINI_DISCOVERY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Gemini transcript directory nesting exceeds the supported limit",
        });
    }
    let metadata = fs::symlink_metadata(path)?;
    if provider_metadata_is_link_like(&metadata) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "linked Gemini transcript path components are rejected",
        });
    }
    if metadata.file_type().is_file() {
        if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            paths.push(path.to_path_buf());
        }
        return Ok(());
    }
    if !metadata.file_type().is_dir() {
        return Ok(());
    }

    let mut children = Vec::new();
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        // Charge both bounds while consuming read_dir, before retaining a
        // PathBuf for deterministic sorting.
        budget.observe(&child)?;
        children.push(child);
    }
    children.sort_unstable();
    for child in children {
        collect_candidates(&child, paths, budget, depth.saturating_add(1))?;
    }
    Ok(())
}

fn gemini_layout_root(root: &Path, metadata: &Metadata) -> PathBuf {
    if let Some(layout_root) = root
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".gemini"))
    {
        return layout_root.to_path_buf();
    }
    for candidate in root.ancestors().skip(1) {
        let Ok(relative) = root.strip_prefix(candidate) else {
            continue;
        };
        let components = relative.components().collect::<Vec<_>>();
        if matches!(
            components.as_slice(),
            [
                Component::Normal(tmp),
                Component::Normal(_project),
                Component::Normal(chats),
                ..
            ] if *tmp == "tmp" && *chats == "chats"
        ) {
            return candidate.to_path_buf();
        }
    }
    if metadata.file_type().is_file() {
        root.parent().unwrap_or(root).to_path_buf()
    } else {
        root.to_path_buf()
    }
}

fn gemini_transcript_layout(
    layout_root: &Path,
    path: &Path,
    direct_file: bool,
) -> Result<Option<GeminiTranscriptLayout>> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
        return Ok(None);
    }
    if let Ok(relative_path) = path.strip_prefix(layout_root) {
        if let Some(layout) = gemini_relative_transcript_layout(path, relative_path)? {
            return Ok(Some(layout));
        }
    }
    if direct_file {
        for ancestor in path.ancestors().skip(1) {
            let Ok(relative_path) = path.strip_prefix(ancestor) else {
                continue;
            };
            if let Some(layout) = gemini_relative_transcript_layout(path, relative_path)? {
                return Ok(Some(layout));
            }
        }
        return Ok(Some(GeminiTranscriptLayout::Primary));
    }
    Ok(None)
}

fn gemini_relative_transcript_layout(
    path: &Path,
    relative_path: &Path,
) -> Result<Option<GeminiTranscriptLayout>> {
    let components: Vec<_> = relative_path.components().collect();
    match components.as_slice() {
        [Component::Normal(tmp), Component::Normal(_project), Component::Normal(chats), Component::Normal(_file)]
            if *tmp == "tmp" && *chats == "chats" =>
        {
            Ok(Some(GeminiTranscriptLayout::Primary))
        }
        [Component::Normal(tmp), Component::Normal(_project), Component::Normal(chats), Component::Normal(parent), Component::Normal(_file)]
            if *tmp == "tmp" && *chats == "chats" =>
        {
            parent
                .to_str()
                .filter(|value| !value.trim().is_empty())
                .map(|value| GeminiTranscriptLayout::Subagent {
                    parent_native_session_id_hint: value.to_owned(),
                })
                .map(Some)
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Gemini subagent transcript parent identity must be nonempty UTF-8",
                })
        }
        _ => Ok(None),
    }
}

fn inventory_digest(transcripts: &[GeminiTranscriptSource]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(INVENTORY_HASH_DOMAIN);
    for transcript in transcripts {
        update_length_prefixed(
            &mut hasher,
            transcript.relative_path.to_string_lossy().as_bytes(),
        );
        hasher.update(transcript.observation.length.to_be_bytes());
        let (side, seconds, nanos) =
            match transcript.observation.modified.duration_since(UNIX_EPOCH) {
                Ok(duration) => (b'+', duration.as_secs(), duration.subsec_nanos()),
                Err(error) => {
                    let duration = error.duration();
                    (b'-', duration.as_secs(), duration.subsec_nanos())
                }
            };
        hasher.update([side]);
        hasher.update(seconds.to_be_bytes());
        hasher.update(nanos.to_be_bytes());
        hasher.update(
            transcript
                .observation
                .device
                .unwrap_or_default()
                .to_be_bytes(),
        );
        hasher.update(
            transcript
                .observation
                .inode
                .unwrap_or_default()
                .to_be_bytes(),
        );
    }
    hasher.finalize().into()
}

fn update_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

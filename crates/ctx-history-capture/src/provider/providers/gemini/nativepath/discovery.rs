use std::{
    fs::Metadata,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use sha2::{Digest, Sha256};

use crate::common::io::{
    open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
    ProviderSourceDirectory, ProviderSourceRoot,
};
use crate::{CaptureError, Result};

use super::dto::{
    GeminiDiscovery, GeminiFileObservation, GeminiTranscriptLayout, GeminiTranscriptSource,
};

pub(super) const MAX_GEMINI_DISCOVERY_DEPTH: usize = 64;
pub(super) const MAX_GEMINI_DISCOVERY_ENTRIES: usize = 100_000;
pub(super) const MAX_GEMINI_DISCOVERY_PATH_BYTES: usize = 64 * 1024 * 1024;
const INVENTORY_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-inventory-v1\0";
type GeminiCatalogCandidate = (PathBuf, PathBuf, GeminiFileObservation, [u8; 32]);

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
    pub(crate) fn from_metadata(metadata: &Metadata) -> Result<Self> {
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

impl GeminiTranscriptSource {
    pub(crate) fn open(&self) -> Result<OpenedProviderSourceFile> {
        let opened = self.authority.open_file(&self.authority_relative_path)?;
        if opened.ordinary_file_token() != self.ordinary_file_token {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(opened)
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
    let canonical_root = root.to_path_buf();
    let opened_root = open_provider_source_path(root)?;
    let root_is_file = matches!(opened_root, OpenedProviderSourcePath::File(_));
    let layout_root = gemini_layout_root(&canonical_root, root_is_file);
    let mut paths = Vec::new();
    let authority = match opened_root {
        OpenedProviderSourcePath::File(file) => {
            budget.observe(&canonical_root)?;
            let parent = canonical_root.parent().ok_or_else(|| {
                CaptureError::InvalidProviderTranscriptPath {
                    path: canonical_root.clone(),
                    reason: "Gemini transcript file has no parent authority",
                }
            })?;
            let relative = canonical_root
                .file_name()
                .map(PathBuf::from)
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: canonical_root.clone(),
                    reason: "Gemini transcript file has no authority-relative name",
                })?;
            let authority = ProviderSourceRoot::open(parent)?;
            if canonical_root
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("jsonl")
            {
                let observation = GeminiFileObservation::from_metadata(file.metadata())?;
                let ordinary_file_token = file.ordinary_file_token();
                file.revalidate_leaf()?;
                let reopened = authority.open_file(&relative)?;
                if reopened.ordinary_file_token() != ordinary_file_token {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                reopened.revalidate_leaf()?;
                paths.push((
                    canonical_root.clone(),
                    relative,
                    observation,
                    ordinary_file_token,
                ));
            } else {
                file.revalidate_leaf()?;
            }
            authority
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let authority = directory.authority_root();
            if let Some((scan_path, scan_directory)) =
                gemini_scan_directory(&canonical_root, directory)?
            {
                budget.observe(&scan_path)?;
                collect_candidates(&scan_path, scan_directory, &mut paths, &mut budget, 0)?;
            }
            authority
        }
    };
    authority.revalidate()?;
    paths.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let mut transcripts = Vec::with_capacity(paths.len());
    for (path, authority_relative_path, observation, ordinary_file_token) in paths {
        let Some(layout) = gemini_transcript_layout(&layout_root, &path, root_is_file)? else {
            continue;
        };
        let relative_path = if root_is_file {
            path.file_name().map(PathBuf::from).unwrap_or_default()
        } else {
            path.strip_prefix(&canonical_root)
                .unwrap_or(&path)
                .to_path_buf()
        };
        transcripts.push(GeminiTranscriptSource {
            observation,
            path,
            relative_path,
            layout,
            ordinary_file_token,
            authority_relative_path,
            authority: authority.clone(),
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

fn gemini_scan_directory(
    root: &Path,
    directory: ProviderSourceDirectory,
) -> Result<Option<(PathBuf, ProviderSourceDirectory)>> {
    let tmp = root.join("tmp");
    let names = directory.entries(MAX_GEMINI_DISCOVERY_ENTRIES)?;
    if names.iter().any(|name| name == "tmp") {
        match open_gemini_child(&directory, std::ffi::OsStr::new("tmp"), &tmp)? {
            OpenedProviderSourcePath::Directory(tmp_directory) => {
                directory.revalidate()?;
                return Ok(Some((tmp, tmp_directory)));
            }
            OpenedProviderSourcePath::File(_) => {}
        }
    }
    if root.file_name().is_some_and(|name| name == ".gemini") {
        directory.revalidate()?;
        Ok(None)
    } else {
        Ok(Some((root.to_path_buf(), directory)))
    }
}

fn collect_candidates(
    path: &Path,
    directory: ProviderSourceDirectory,
    paths: &mut Vec<GeminiCatalogCandidate>,
    budget: &mut DiscoveryBudget,
    depth: usize,
) -> Result<()> {
    if depth > MAX_GEMINI_DISCOVERY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Gemini transcript directory nesting exceeds the supported limit",
        });
    }
    let children = directory.entries(MAX_GEMINI_DISCOVERY_ENTRIES)?;
    for name in children {
        let child = path.join(&name);
        // Charge both bounds while consuming read_dir, before retaining a
        // PathBuf for deterministic sorting.
        budget.observe(&child)?;
        match open_gemini_child(&directory, &name, &child)? {
            OpenedProviderSourcePath::Directory(child_directory) => collect_candidates(
                &child,
                child_directory,
                paths,
                budget,
                depth.saturating_add(1),
            )?,
            OpenedProviderSourcePath::File(file)
                if child.extension().and_then(|extension| extension.to_str()) == Some("jsonl") =>
            {
                let observation = GeminiFileObservation::from_metadata(file.metadata())?;
                let ordinary_file_token = file.ordinary_file_token();
                file.revalidate_leaf()?;
                paths.push((
                    child,
                    directory.relative_path().join(name),
                    observation,
                    ordinary_file_token,
                ));
            }
            OpenedProviderSourcePath::File(file) => file.revalidate_leaf()?,
        }
    }
    directory.revalidate()?;
    Ok(())
}

fn open_gemini_child(
    directory: &ProviderSourceDirectory,
    name: &std::ffi::OsStr,
    path: &Path,
) -> Result<OpenedProviderSourcePath> {
    directory.open_child(name).map_err(|error| match error {
        CaptureError::InvalidProviderTranscriptPath { .. } => {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "linked Gemini transcript path components are rejected",
            }
        }
        error => error,
    })
}

fn gemini_layout_root(root: &Path, root_is_file: bool) -> PathBuf {
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
    if root_is_file {
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
        hasher.update(transcript.ordinary_file_token);
    }
    hasher.finalize().into()
}

fn update_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

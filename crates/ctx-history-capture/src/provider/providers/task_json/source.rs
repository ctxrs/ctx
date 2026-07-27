use std::{
    fs::{self, Metadata},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::{fnv1a64, CaptureError, ProviderAdapterContext, Result, MAX_PROVIDER_JSONL_LINE_BYTES};

use super::dialect::{
    TaskJsonMessagePhase, TaskJsonProviderSpec, TASK_JSON_CAPTURE_REVISION,
    TASK_JSON_POLICY_REVISION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskJsonFrozenFile {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl TaskJsonFrozenFile {
    pub(super) fn read(path: &Path) -> Result<Self> {
        ensure_provider_path_parents_are_not_symlinks(path)?;
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
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

    fn revision_component(&self, output: &mut String) {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        output.push_str(&format!(
            "{}\0{side}{seconds}.{nanos:09}\0{}\0{:?}\0{:?}",
            self.length, self.readonly, self.device, self.inode,
        ));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskJsonObservedFile {
    pub(super) path: PathBuf,
    pub(super) frozen: Option<TaskJsonFrozenFile>,
}

impl TaskJsonObservedFile {
    pub(super) fn read(path: PathBuf) -> Result<Self> {
        let frozen = match fs::symlink_metadata(&path) {
            Ok(_) => Some(TaskJsonFrozenFile::read(&path)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, frozen })
    }

    fn revision_component(&self, label: &str, output: &mut String) {
        output.push_str(label);
        output.push('\0');
        output.push_str(&format!("{:?}\0", self.path.as_os_str()));
        match &self.frozen {
            Some(frozen) => frozen.revision_component(output),
            None => output.push_str("missing"),
        }
        output.push('\n');
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        Ok(Self::read(self.path.clone())? == *self)
    }
}

#[derive(Debug, Clone)]
pub(super) struct TaskJsonTaskObservation {
    pub(super) canonical_task_dir: PathBuf,
    pub(super) marker_files: Vec<TaskJsonObservedFile>,
    pub(super) root_history_files: Vec<TaskJsonObservedFile>,
}

impl TaskJsonTaskObservation {
    pub(super) fn read(
        task_dir: &Path,
        root_history_paths: &[PathBuf],
        spec: TaskJsonProviderSpec,
    ) -> Result<Self> {
        ensure_provider_path_parents_are_not_symlinks(task_dir)?;
        let mut marker_files = Vec::new();
        for file_name in task_json_marker_file_names(spec) {
            marker_files.push(TaskJsonObservedFile::read(task_dir.join(file_name))?);
        }
        let mut root_history_files = Vec::new();
        for path in root_history_paths {
            root_history_files.push(TaskJsonObservedFile::read(path.clone())?);
        }
        Ok(Self {
            canonical_task_dir: fs::canonicalize(task_dir)?,
            marker_files,
            root_history_files,
        })
    }

    pub(super) fn source_revision(&self, spec: TaskJsonProviderSpec) -> String {
        let mut input = format!(
            "task-json-task-v1\0capture={TASK_JSON_CAPTURE_REVISION}\0policy={TASK_JSON_POLICY_REVISION}\n"
        );
        for (file_name, file) in task_json_marker_file_names(spec)
            .into_iter()
            .zip(&self.marker_files)
        {
            file.revision_component(file_name, &mut input);
        }
        for file in &self.root_history_files {
            file.revision_component("root-history", &mut input);
        }
        format!(
            "task-json-task-v1:fnv1a64:{:016x}",
            fnv1a64(input.as_bytes())
        )
    }

    pub(super) fn marker_file(
        &self,
        spec: TaskJsonProviderSpec,
        file_name: &str,
    ) -> Option<&TaskJsonObservedFile> {
        task_json_marker_file_names(spec)
            .into_iter()
            .position(|candidate| candidate == file_name)
            .and_then(|index| self.marker_files.get(index))
    }

    pub(super) fn message_file(
        &self,
        spec: TaskJsonProviderSpec,
        phase: TaskJsonMessagePhase,
    ) -> Option<&TaskJsonObservedFile> {
        phase
            .file_name(spec)
            .and_then(|file_name| self.marker_file(spec, file_name))
            .filter(|file| file.frozen.is_some())
    }

    pub(super) fn revalidate(&self, task_dir: &Path) -> Result<bool> {
        if fs::canonicalize(task_dir)? != self.canonical_task_dir {
            return Ok(false);
        }
        for file in self.marker_files.iter().chain(&self.root_history_files) {
            if !file.revalidate()? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn task_json_marker_file_names(spec: TaskJsonProviderSpec) -> Vec<&'static str> {
    let mut files = vec![spec.metadata_file, spec.api_file, spec.ui_file];
    if let Some(file) = spec.history_item_file {
        files.push(file);
    }
    if let Some(file) = spec.index_file {
        files.push(file);
    }
    if let Some(file) = spec.fallback_api_file {
        files.push(file);
    }
    files
}

pub(super) fn visit_task_json_dirs(
    path: &Path,
    spec: TaskJsonProviderSpec,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(path)?;
    if file_type.is_file() {
        ensure_regular_provider_transcript_file(path)?;
        if task_json_file_name_is_marker(path, spec) {
            if let Some(parent) = path.parent() {
                visit(parent)?;
                return Ok(1);
            }
        }
        return Ok(0);
    }
    if !file_type.is_dir() {
        return Ok(0);
    }
    if task_json_dir_has_marker(path, spec) {
        visit(path)?;
        return Ok(1);
    }

    let mut count = 0_usize;
    let tasks = path.join("tasks");
    for root in [&tasks, path] {
        if !root.is_dir() {
            continue;
        }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let candidate = entry.path();
            if task_json_dir_has_marker(&candidate, spec) {
                visit(&candidate)?;
                count = count.saturating_add(1);
            }
        }
    }
    Ok(count)
}

pub(super) fn task_json_root_history_candidate_paths(
    path: &Path,
    spec: TaskJsonProviderSpec,
) -> Vec<PathBuf> {
    if spec.provider != CaptureProvider::Cline {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    if path.is_dir() {
        candidates.push(path.join("state").join("taskHistory.json"));
        candidates.push(path.join("..").join("state").join("taskHistory.json"));
    }
    if let Some(parent) = path.parent() {
        candidates.push(parent.join("state").join("taskHistory.json"));
        if let Some(grandparent) = parent.parent() {
            candidates.push(grandparent.join("state").join("taskHistory.json"));
        }
    }
    let mut unique = Vec::new();
    for candidate in candidates {
        if !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
}

pub(super) fn task_json_missing_reason(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::RooCode => {
            "no Roo Code task JSON directories with api_conversation_history.json, ui_messages.json, history_item.json, _index.json, or claude_messages.json were found"
        }
        _ => {
            "no Cline task JSON directories with api_conversation_history.json, ui_messages.json, or task_metadata.json were found"
        }
    }
}

fn task_json_file_name_is_marker(path: &Path, spec: TaskJsonProviderSpec) -> bool {
    let name = path.file_name().and_then(|name| name.to_str());
    name == Some(spec.api_file)
        || name == Some(spec.ui_file)
        || name == Some(spec.metadata_file)
        || spec
            .history_item_file
            .is_some_and(|file| name == Some(file))
        || spec.index_file.is_some_and(|file| name == Some(file))
        || spec
            .fallback_api_file
            .is_some_and(|file| name == Some(file))
}

fn task_json_dir_has_marker(path: &Path, spec: TaskJsonProviderSpec) -> bool {
    path.join(spec.api_file).is_file()
        || path.join(spec.ui_file).is_file()
        || path.join(spec.metadata_file).is_file()
        || spec
            .history_item_file
            .is_some_and(|file| path.join(file).is_file())
        || spec
            .index_file
            .is_some_and(|file| path.join(file).is_file())
        || spec
            .fallback_api_file
            .is_some_and(|file| path.join(file).is_file())
}

pub(super) fn read_task_json_value(
    path: &Path,
    _context: &ProviderAdapterContext,
) -> Result<Value> {
    ensure_regular_provider_transcript_file(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PROVIDER_JSONL_LINE_BYTES as u64 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider task JSON file exceeds maximum supported size",
        });
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(CaptureError::from)
}

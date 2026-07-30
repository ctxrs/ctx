use std::{
    collections::HashSet,
    io::{BufReader, Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::common::io::{
    open_provider_source_path, read_provider_jsonl_line_or_skip_oversized,
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderJsonlLineRead, ProviderSourceRoot,
};
use crate::provider::normalization::provider_local_preview;
use crate::provider::provider_safe_path_segment;
use crate::{CaptureError, ProviderImportFailure, Result, PROVIDER_MAX_PREVIEW_CHARS};

use super::{
    MAX_JUNIE_FAILURES, MAX_JUNIE_FAILURE_BYTES, MAX_JUNIE_INDEX_BYTES, MAX_JUNIE_INDEX_ENTRIES,
    MAX_JUNIE_INDEX_METADATA_BYTES,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JunieIndexMeta {
    pub(super) session_id: String,
    pub(super) created_at: Option<i64>,
    pub(super) updated_at: Option<i64>,
    pub(super) task_name: Option<String>,
    pub(super) project_dir: Option<String>,
    pub(super) raw: Value,
}

#[derive(Debug, Clone)]
pub(super) struct JunieSessionPath {
    pub(super) events_path: PathBuf,
    pub(super) index_meta: JunieIndexMeta,
    pub(super) require_supported_events: bool,
    authority: ProviderSourceRoot,
    events_relative: PathBuf,
    index_authority: Option<ProviderSourceRoot>,
    index_relative: Option<PathBuf>,
}

impl JunieSessionPath {
    pub(super) fn open_events(&self) -> Result<OpenedProviderSourceFile> {
        self.authority.open_file(&self.events_relative)
    }

    pub(super) fn open_index(&self) -> Result<Option<OpenedProviderSourceFile>> {
        let (Some(authority), Some(relative)) = (
            self.index_authority.as_ref(),
            self.index_relative.as_deref(),
        ) else {
            return Ok(None);
        };
        match authority.open_file(relative) {
            Ok(file) => Ok(Some(file)),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn revalidate_root(&self) -> Result<()> {
        self.authority.revalidate()?;
        if let Some(index_authority) = &self.index_authority {
            index_authority.revalidate()?;
        }
        Ok(())
    }
}

struct JunieIndex {
    ordered_metas: Vec<JunieIndexMeta>,
    session_ids: HashSet<String>,
    rejection_count: u64,
    rejections: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct JunieSessionTreeVisit {
    pub(super) rejection_count: u64,
}

pub(super) fn visit_junie_session_event_paths(
    path: &Path,
    visit: &mut dyn FnMut(JunieSessionPath, usize) -> Result<()>,
) -> Result<JunieSessionTreeVisit> {
    let requested = normalized_junie_authority_path(path)?;
    let opened = open_provider_source_path(&requested)?;
    if let OpenedProviderSourcePath::File(file) = opened {
        if requested.file_name().and_then(|name| name.to_str()) != Some("events.jsonl") {
            file.revalidate()?;
            return Ok(JunieSessionTreeVisit::default());
        }
        let session_dir =
            requested
                .parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: requested.clone(),
                    reason: "Junie events.jsonl path has no session directory",
                })?;
        let index_root =
            session_dir
                .parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: requested.clone(),
                    reason: "Junie events.jsonl path has no retained tree root",
                })?;
        let authority = ProviderSourceRoot::open(session_dir)?;
        let index_authority = ProviderSourceRoot::open(index_root)?;
        let events_relative = PathBuf::from("events.jsonl");
        authority.open_file(&events_relative)?.revalidate()?;
        file.revalidate()?;
        let events_path = authority.named_path().join(&events_relative);
        let session_id = junie_session_id_from_events_path(&events_path)?;
        let index_relative = PathBuf::from("index.jsonl");
        let index_meta =
            junie_index_meta_for_authority(&index_authority, &index_relative, &session_id)
                .unwrap_or_else(|| JunieIndexMeta {
                    session_id,
                    ..JunieIndexMeta::default()
                });
        visit(
            JunieSessionPath {
                events_path,
                index_meta,
                require_supported_events: true,
                authority: authority.clone(),
                events_relative,
                index_authority: Some(index_authority.clone()),
                index_relative: Some(index_relative),
            },
            0,
        )?;
        authority.revalidate()?;
        index_authority.revalidate()?;
        return Ok(JunieSessionTreeVisit::default());
    }
    let OpenedProviderSourcePath::Directory(selected_directory) = opened else {
        return Err(CaptureError::SystemInvariant(
            "Junie root classification is incomplete",
        ));
    };
    match selected_directory.open_child(std::ffi::OsStr::new("events.jsonl")) {
        Ok(OpenedProviderSourcePath::File(events)) => {
            let parent =
                requested
                    .parent()
                    .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                        path: requested.clone(),
                        reason: "Junie session directory has no retained tree parent",
                    })?;
            let authority = selected_directory.authority_root();
            let index_authority = ProviderSourceRoot::open(parent)?;
            let events_relative = PathBuf::from("events.jsonl");
            authority.open_file(&events_relative)?.revalidate()?;
            events.revalidate()?;
            let events_path = authority.named_path().join(&events_relative);
            let session_id = junie_session_id_from_events_path(&events_path)?;
            let index_relative = PathBuf::from("index.jsonl");
            let index_meta =
                junie_index_meta_for_authority(&index_authority, &index_relative, &session_id)
                    .unwrap_or_else(|| JunieIndexMeta {
                        session_id,
                        ..JunieIndexMeta::default()
                    });
            visit(
                JunieSessionPath {
                    events_path,
                    index_meta,
                    require_supported_events: true,
                    authority: authority.clone(),
                    events_relative,
                    index_authority: Some(index_authority.clone()),
                    index_relative: Some(index_relative),
                },
                0,
            )?;
            authority.revalidate()?;
            index_authority.revalidate()?;
            return Ok(JunieSessionTreeVisit::default());
        }
        Ok(OpenedProviderSourcePath::Directory(directory)) => directory.revalidate()?,
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let authority = selected_directory.authority_root();
    let index_relative = PathBuf::from("index.jsonl");
    let JunieIndex {
        ordered_metas,
        session_ids: indexed_session_ids,
        rejection_count,
        rejections,
    } = match read_junie_index(&authority, &index_relative) {
        Ok(index) => index,
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            authority.revalidate()?;
            return Ok(JunieSessionTreeVisit::default());
        }
        Err(error) => return Err(error),
    };
    let mut visited = 0_usize;
    for meta in ordered_metas {
        let events_relative = PathBuf::from(&meta.session_id).join("events.jsonl");
        let events = match authority.open_path(&events_relative) {
            Ok(OpenedProviderSourcePath::File(events)) => events,
            Ok(OpenedProviderSourcePath::Directory(directory)) => {
                directory.revalidate()?;
                continue;
            }
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                continue
            }
            Err(error) => return Err(error),
        };
        events.revalidate()?;
        let ordinal = visited;
        visit(
            JunieSessionPath {
                events_path: authority.named_path().join(&events_relative),
                index_meta: meta,
                require_supported_events: true,
                authority: authority.clone(),
                events_relative,
                index_authority: Some(authority.clone()),
                index_relative: Some(index_relative.clone()),
            },
            ordinal,
        )?;
        visited = visited.saturating_add(1);
    }

    let names = selected_directory.entries(MAX_JUNIE_INDEX_ENTRIES.saturating_add(1))?;
    if names.len() > MAX_JUNIE_INDEX_ENTRIES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: requested,
            reason: "Junie session tree exceeds the bounded directory entry limit",
        });
    }
    for name in names {
        let Some(session_id) = name.to_str().map(str::to_owned) else {
            continue;
        };
        if !junie_session_id_is_safe(&session_id) {
            continue;
        }
        let Ok(OpenedProviderSourcePath::Directory(session_dir)) =
            selected_directory.open_child(&name)
        else {
            continue;
        };
        if indexed_session_ids.contains(&session_id) {
            session_dir.revalidate()?;
            continue;
        }
        let events = match session_dir.open_child(std::ffi::OsStr::new("events.jsonl")) {
            Ok(OpenedProviderSourcePath::File(events)) => events,
            Ok(OpenedProviderSourcePath::Directory(directory)) => {
                directory.revalidate()?;
                session_dir.revalidate()?;
                continue;
            }
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                session_dir.revalidate()?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let events_relative = PathBuf::from(&session_id).join("events.jsonl");
        events.revalidate()?;
        session_dir.revalidate()?;
        let ordinal = visited;
        visit(
            JunieSessionPath {
                events_path: authority.named_path().join(&events_relative),
                index_meta: JunieIndexMeta {
                    session_id,
                    ..JunieIndexMeta::default()
                },
                require_supported_events: false,
                authority: authority.clone(),
                events_relative,
                index_authority: Some(authority.clone()),
                index_relative: Some(index_relative.clone()),
            },
            ordinal,
        )?;
        visited = visited.saturating_add(1);
    }
    selected_directory.revalidate()?;
    authority.revalidate()?;
    let _ = (visited, rejections);
    Ok(JunieSessionTreeVisit { rejection_count })
}

pub(super) fn junie_provider_session_id(session_path: &JunieSessionPath) -> Result<String> {
    let provider_session_id = if session_path.index_meta.session_id.is_empty() {
        junie_session_id_from_events_path(&session_path.events_path)?
    } else {
        session_path.index_meta.session_id.clone()
    };
    if !junie_session_id_is_safe(&provider_session_id) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: session_path.events_path.clone(),
            reason: "Junie session id is not a safe path segment",
        });
    }
    Ok(provider_session_id)
}

pub(super) fn bounded_junie_index_meta(meta: &JunieIndexMeta) -> JunieIndexMeta {
    let session_id = provider_local_preview(&meta.session_id, PROVIDER_MAX_PREVIEW_CHARS).0;
    let task_name = meta
        .task_name
        .as_deref()
        .map(|value| provider_local_preview(value, PROVIDER_MAX_PREVIEW_CHARS).0);
    let project_dir = meta
        .project_dir
        .as_deref()
        .map(|value| provider_local_preview(value, PROVIDER_MAX_PREVIEW_CHARS).0);
    let raw = bounded_junie_metadata(&meta.raw).unwrap_or_else(|| {
        json!({
            "sessionId": &session_id,
            "createdAt": meta.created_at,
            "updatedAt": meta.updated_at,
            "taskName": task_name.as_deref(),
            "projectDir": project_dir.as_deref(),
            "ctxTruncated": true,
        })
    });
    JunieIndexMeta {
        session_id,
        created_at: meta.created_at,
        updated_at: meta.updated_at,
        task_name,
        project_dir,
        raw,
    }
}

fn bounded_junie_metadata(value: &Value) -> Option<Value> {
    let mut writer = BoundedJsonWriter {
        bytes: Vec::new(),
        maximum: MAX_JUNIE_INDEX_METADATA_BYTES,
    };
    serde_json::to_writer(&mut writer, value).ok()?;
    serde_json::from_slice(&writer.bytes).ok()
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .ok_or_else(|| std::io::Error::other("Junie metadata length overflow"))?;
        if next > self.maximum {
            return Err(std::io::Error::other(
                "Junie metadata exceeds its byte bound",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn junie_index_meta_for_authority(
    authority: &ProviderSourceRoot,
    index_relative: &Path,
    session_id: &str,
) -> Option<JunieIndexMeta> {
    read_junie_index(authority, index_relative)
        .ok()?
        .ordered_metas
        .into_iter()
        .find(|meta| meta.session_id == session_id)
}

fn read_junie_index(authority: &ProviderSourceRoot, relative: &Path) -> Result<JunieIndex> {
    let opened = authority.open_file(relative)?;
    let mut reader = BufReader::new(opened.file().try_clone()?);
    let mut line = Vec::new();
    let mut entry_count = 0_usize;
    let mut total_bytes = 0_usize;
    let mut ordered_metas = Vec::new();
    let mut session_ids = HashSet::new();
    let mut rejection_count = 0_u64;
    let mut rejections = Vec::new();
    loop {
        let remaining_bytes = MAX_JUNIE_INDEX_BYTES.saturating_sub(total_bytes);
        let read_limit = u64::try_from(remaining_bytes.saturating_add(1))
            .map_err(|_| CaptureError::SystemInvariant("Junie index byte limit exceeds u64"))?;
        let mut bounded_reader = (&mut reader).take(read_limit);
        let read = read_provider_jsonl_line_or_skip_oversized(&mut bounded_reader, &mut line)?;
        let bytes = match read {
            ProviderJsonlLineRead::Eof => break,
            ProviderJsonlLineRead::Line { bytes } | ProviderJsonlLineRead::Oversized { bytes } => {
                bytes
            }
        };
        entry_count = entry_count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie index entry count overflowed",
            ))?;
        if entry_count > MAX_JUNIE_INDEX_ENTRIES {
            return Err(CaptureError::InvalidPayload(format!(
                "Junie index exceeds the {MAX_JUNIE_INDEX_ENTRIES} entry limit"
            )));
        }
        total_bytes = total_bytes
            .checked_add(bytes)
            .ok_or(CaptureError::SystemInvariant(
                "Junie index byte count overflowed",
            ))?;
        if total_bytes > MAX_JUNIE_INDEX_BYTES {
            return Err(CaptureError::InvalidPayload(format!(
                "Junie index exceeds the {MAX_JUNIE_INDEX_BYTES} byte limit"
            )));
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if matches!(read, ProviderJsonlLineRead::Oversized { .. }) {
            record_index_rejection(
                &mut rejections,
                &mut rejection_count,
                entry_count,
                format!(
                    "Junie index row exceeds the {} byte provider record limit",
                    crate::MAX_PROVIDER_JSONL_LINE_BYTES
                ),
            );
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            record_index_rejection(
                &mut rejections,
                &mut rejection_count,
                entry_count,
                "Junie index row is not valid JSON".to_owned(),
            );
            continue;
        };
        let Some(meta) = junie_index_meta_from_value(value) else {
            record_index_rejection(
                &mut rejections,
                &mut rejection_count,
                entry_count,
                "Junie index row has a missing or unsafe sessionId".to_owned(),
            );
            continue;
        };
        if session_ids.insert(meta.session_id.clone()) {
            ordered_metas.push(meta);
        }
    }
    opened.revalidate()?;
    authority.revalidate()?;
    Ok(JunieIndex {
        ordered_metas,
        session_ids,
        rejection_count,
        rejections,
    })
}

fn normalized_junie_authority_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(CaptureError::InvalidProviderTranscriptPath {
                        path: path.to_path_buf(),
                        reason: "Junie root cannot escape the filesystem root",
                    });
                }
            }
        }
    }
    Ok(normalized)
}

fn record_index_rejection(
    failures: &mut Vec<ProviderImportFailure>,
    rejection_count: &mut u64,
    line: usize,
    mut error: String,
) {
    *rejection_count = rejection_count.saturating_add(1);
    if failures.len() >= MAX_JUNIE_FAILURES {
        return;
    }
    if error.len() > MAX_JUNIE_FAILURE_BYTES {
        let mut boundary = MAX_JUNIE_FAILURE_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        error.truncate(boundary);
    }
    failures.push(ProviderImportFailure { line, error });
}

fn junie_index_meta_from_value(value: Value) -> Option<JunieIndexMeta> {
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|session_id| junie_session_id_is_safe(session_id))?
        .to_owned();
    Some(JunieIndexMeta {
        session_id,
        created_at: junie_timestamp_millis_field(&value, "createdAt"),
        updated_at: junie_timestamp_millis_field(&value, "updatedAt"),
        task_name: value
            .get("taskName")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        project_dir: value
            .get("projectDir")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        raw: value,
    })
}

pub(super) fn junie_timestamp_millis_field(value: &Value, field: &str) -> Option<i64> {
    let value = value.get(field)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
}

pub(super) fn junie_session_id_from_events_path(path: &Path) -> Result<String> {
    let Some(session_id) = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Junie events.jsonl path is not inside a session directory",
        });
    };
    if !junie_session_id_is_safe(session_id) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Junie session id is not a safe path segment",
        });
    }
    Ok(session_id.to_owned())
}

pub(super) fn junie_session_id_is_safe(session_id: &str) -> bool {
    provider_safe_path_segment(session_id)
}

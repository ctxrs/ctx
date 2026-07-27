use std::{
    collections::HashSet,
    fs,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead,
};
use crate::provider::importer::BoundedParserCheckpoint;
use crate::provider::normalization::provider_local_preview;
use crate::provider::provider_safe_path_segment;
use crate::{CaptureError, ProviderImportFailure, Result, PROVIDER_MAX_PREVIEW_CHARS};

use super::{
    MAX_JUNIE_FAILURES, MAX_JUNIE_FAILURE_BYTES, MAX_JUNIE_INDEX_BYTES, MAX_JUNIE_INDEX_ENTRIES,
    MAX_JUNIE_INDEX_METADATA_BYTES,
};

#[derive(Debug, Clone, Default)]
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
}

struct JunieIndex {
    ordered_metas: Vec<JunieIndexMeta>,
    session_ids: HashSet<String>,
    rejection_count: u64,
    rejections: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct JunieSessionTreeVisit {
    pub(super) visited: usize,
    pub(super) rejection_count: u64,
    pub(super) rejections: Vec<ProviderImportFailure>,
}

pub(super) fn visit_junie_session_event_paths(
    path: &Path,
    visit: &mut dyn FnMut(JunieSessionPath, usize) -> Result<()>,
) -> Result<JunieSessionTreeVisit> {
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
        if path.file_name().and_then(|name| name.to_str()) != Some("events.jsonl") {
            return Ok(JunieSessionTreeVisit::default());
        }
        let session_id = junie_session_id_from_events_path(path)?;
        let index_meta =
            junie_index_meta_for_events_path(path, &session_id).unwrap_or_else(|| JunieIndexMeta {
                session_id,
                ..JunieIndexMeta::default()
            });
        visit(
            JunieSessionPath {
                events_path: path.to_path_buf(),
                index_meta,
                require_supported_events: true,
            },
            0,
        )?;
        return Ok(JunieSessionTreeVisit {
            visited: 1,
            ..JunieSessionTreeVisit::default()
        });
    }
    if !file_type.is_dir() {
        return Ok(JunieSessionTreeVisit::default());
    }

    let direct_events = path.join("events.jsonl");
    if direct_events.is_file() {
        ensure_regular_provider_transcript_file(&direct_events)?;
        let session_id = junie_session_id_from_events_path(&direct_events)?;
        let index_meta = junie_index_meta_for_events_path(&direct_events, &session_id)
            .unwrap_or_else(|| JunieIndexMeta {
                session_id,
                ..JunieIndexMeta::default()
            });
        visit(
            JunieSessionPath {
                events_path: direct_events,
                index_meta,
                require_supported_events: true,
            },
            0,
        )?;
        return Ok(JunieSessionTreeVisit {
            visited: 1,
            ..JunieSessionTreeVisit::default()
        });
    }

    let index_path = path.join("index.jsonl");
    if !index_path.is_file() {
        return Ok(JunieSessionTreeVisit::default());
    }
    let JunieIndex {
        ordered_metas,
        session_ids: indexed_session_ids,
        rejection_count,
        rejections,
    } = read_junie_index(&index_path)?;
    let mut visited = 0_usize;
    for meta in ordered_metas {
        let events_path = path.join(&meta.session_id).join("events.jsonl");
        if events_path.is_file() {
            ensure_regular_provider_transcript_file(&events_path)?;
            let ordinal = visited;
            visit(
                JunieSessionPath {
                    events_path,
                    index_meta: meta,
                    require_supported_events: true,
                },
                ordinal,
            )?;
            visited = visited.saturating_add(1);
        }
    }

    let mut previous_session_id: Option<String> = None;
    loop {
        let mut next_session: Option<(String, PathBuf)> = None;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(session_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !junie_session_id_is_safe(&session_id)
                || previous_session_id
                    .as_ref()
                    .is_some_and(|previous| session_id.as_str() <= previous.as_str())
                || next_session
                    .as_ref()
                    .is_some_and(|(next, _)| session_id.as_str() >= next.as_str())
            {
                continue;
            }
            next_session = Some((session_id, entry.path()));
        }
        let Some((session_id, session_dir)) = next_session else {
            break;
        };
        previous_session_id = Some(session_id.clone());
        if indexed_session_ids.contains(&session_id) {
            continue;
        }
        let events_path = session_dir.join("events.jsonl");
        if !events_path.is_file() {
            continue;
        }
        ensure_regular_provider_transcript_file(&events_path)?;
        let ordinal = visited;
        visit(
            JunieSessionPath {
                events_path,
                index_meta: JunieIndexMeta {
                    session_id,
                    ..JunieIndexMeta::default()
                },
                require_supported_events: false,
            },
            ordinal,
        )?;
        visited = visited.saturating_add(1);
    }
    Ok(JunieSessionTreeVisit {
        visited,
        rejection_count,
        rejections,
    })
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

pub(super) fn junie_index_path_for_events(path: &Path) -> Option<PathBuf> {
    Some(path.parent()?.parent()?.join("index.jsonl"))
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
    let raw = BoundedParserCheckpoint::from_serializable(&meta.raw)
        .ok()
        .filter(|checkpoint| checkpoint.as_bytes().len() <= MAX_JUNIE_INDEX_METADATA_BYTES)
        .and_then(|checkpoint| checkpoint.deserialize::<Value>().ok())
        .unwrap_or_else(|| {
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

pub(super) fn junie_index_meta_for_events_path(
    path: &Path,
    session_id: &str,
) -> Option<JunieIndexMeta> {
    let index_path = junie_index_path_for_events(path)?;
    read_junie_index(&index_path)
        .ok()?
        .ordered_metas
        .into_iter()
        .find(|meta| meta.session_id == session_id)
}

fn read_junie_index(path: &Path) -> Result<JunieIndex> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
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
    Ok(JunieIndex {
        ordered_metas,
        session_ids,
        rejection_count,
        rejections,
    })
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

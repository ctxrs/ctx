use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::common::io::{ensure_regular_provider_transcript_file, read_json_file_limited};
use crate::provider::provider_safe_path_segment;
use crate::provider::providers::task_json::{task_json_string_field, task_json_time_field};
use crate::{
    CaptureError, ProviderImportFailure, ProviderImportSummary, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::super::source::{CodeBuddyFrozenFile, CodeBuddyRevisionHasher};
use super::super::{
    CODEBUDDY_CAPTURE_REVISION, CODEBUDDY_MAX_CHECKPOINT_TEXT_BYTES, CODEBUDDY_POLICY_REVISION,
};

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyExtensionMetadata {
    pub(super) session_dir: PathBuf,
    pub(super) project_dir: PathBuf,
    pub(super) native_session_id: String,
    pub(super) project_hash: String,
    pub(super) project_index: Option<Value>,
    pub(super) conversation: Option<Value>,
    pub(super) session_index: Value,
}

impl CodeBuddyExtensionMetadata {
    pub(super) fn messages(&self) -> &[Value] {
        self.session_index
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyExtensionObservation {
    pub(super) canonical_session_dir: PathBuf,
    pub(super) source_revision: String,
}

impl CodeBuddyExtensionObservation {
    pub(super) fn read(
        metadata: &CodeBuddyExtensionMetadata,
        session_ordinal: usize,
        summary: &mut ProviderImportSummary,
    ) -> Result<Self> {
        let canonical_session_dir = fs::canonicalize(&metadata.session_dir)?;
        let (source_revision, _) =
            codebuddy_extension_source_revision(metadata, session_ordinal, Some(summary))?;
        Ok(Self {
            canonical_session_dir,
            source_revision,
        })
    }
}

pub(super) fn codebuddy_extension_metadata(
    session_dir: &Path,
    session_ordinal: usize,
) -> Result<(Option<CodeBuddyExtensionMetadata>, ProviderImportSummary)> {
    let mut summary = ProviderImportSummary::default();
    let session_index_path = session_dir.join("index.json");
    let session_index =
        match ensure_regular_provider_transcript_file(&session_index_path).and_then(|_| {
            read_json_file_limited(
                &session_index_path,
                MAX_PROVIDER_JSONL_LINE_BYTES,
                "CodeBuddy session index.json",
            )
        }) {
            Ok(value) => value,
            Err(error) => {
                summary.record_failure(ProviderImportFailure {
                    line: session_ordinal,
                    error: format!("index.json: {error}"),
                });
                return Ok((None, summary));
            }
        };
    let project_dir = session_dir.parent().unwrap_or(session_dir).to_path_buf();
    let project_hash = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-project")
        .to_owned();
    let native_session_id = session_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-session")
        .to_owned();
    let (project_index, conversation) = codebuddy_project_index_and_conversation(
        &project_dir,
        &native_session_id,
        &mut summary,
        session_ordinal,
    );
    Ok((
        Some(CodeBuddyExtensionMetadata {
            session_dir: session_dir.to_path_buf(),
            project_dir,
            native_session_id,
            project_hash,
            project_index,
            conversation,
            session_index,
        }),
        summary,
    ))
}

fn codebuddy_extension_source_revision(
    metadata: &CodeBuddyExtensionMetadata,
    session_ordinal: usize,
    mut summary: Option<&mut ProviderImportSummary>,
) -> Result<(String, u64)> {
    let mut revision = CodeBuddyRevisionHasher::new();
    revision.update(b"codebuddy-extension-source-v1");
    revision.update(&CODEBUDDY_CAPTURE_REVISION.to_be_bytes());
    revision.update(&CODEBUDDY_POLICY_REVISION.to_be_bytes());
    serde_json::to_writer(&mut revision, &metadata.session_index)?;
    serde_json::to_writer(&mut revision, &metadata.project_index)?;
    codebuddy_hash_path_state(&mut revision, &metadata.session_dir.join("index.json"))?;
    codebuddy_hash_path_state(&mut revision, &metadata.project_dir.join("index.json"))?;
    codebuddy_hash_path_state(&mut revision, &metadata.session_dir.join("messages"))?;

    let mut record_count = 0_u64;
    for (message_index, message_ref) in metadata.messages().iter().enumerate() {
        revision.update(&(message_index as u64).to_be_bytes());
        serde_json::to_writer(&mut revision, message_ref)?;
        match codebuddy_extension_message_file(&metadata.session_dir, message_ref) {
            Ok((path, file)) => {
                revision.update(b"regular-message");
                file.update_revision(&mut revision);
                record_count = record_count
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy extension record count overflowed",
                    ))?;
                revision.update(path.as_os_str().as_encoded_bytes());
            }
            Err(error) => {
                revision.update(b"rejected-message");
                revision.update(error.as_bytes());
                if let Some(summary) = summary.as_deref_mut() {
                    summary.record_failure(ProviderImportFailure {
                        line: codebuddy_extension_line_number(session_ordinal, message_index),
                        error,
                    });
                }
            }
        }
    }
    Ok((
        format!(
            "codebuddy-extension-source-v1:fnv1a64:{:016x}",
            revision.finish()
        ),
        record_count,
    ))
}

fn codebuddy_hash_path_state(revision: &mut CodeBuddyRevisionHasher, path: &Path) -> Result<()> {
    revision.update(path.as_os_str().as_encoded_bytes());
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            revision.update(b"metadata");
            revision.update(&[u8::from(metadata.file_type().is_file())]);
            revision.update(&[u8::from(metadata.file_type().is_dir())]);
            revision.update(&[u8::from(metadata.file_type().is_symlink())]);
            CodeBuddyFrozenFile::from_metadata(&metadata)?.update_revision(revision);
        }
        Err(error) => {
            revision.update(b"metadata-error");
            revision.update(error.kind().to_string().as_bytes());
            revision.update(&error.raw_os_error().unwrap_or_default().to_be_bytes());
        }
    }
    Ok(())
}

pub(super) fn codebuddy_extension_message_file(
    session_dir: &Path,
    message_ref: &Value,
) -> std::result::Result<(PathBuf, CodeBuddyFrozenFile), String> {
    let Some(message_id) = message_ref
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
    else {
        return Err("CodeBuddy message ref has empty id".to_owned());
    };
    if !provider_safe_path_segment(message_id) {
        return Err("CodeBuddy message ref id is not a safe path segment".to_owned());
    }
    let path = session_dir
        .join("messages")
        .join(format!("{message_id}.json"));
    let file = CodeBuddyFrozenFile::read(&path)
        .map_err(|error| format!("messages/{message_id}.json: {error}"))?;
    if file.length > MAX_PROVIDER_JSONL_LINE_BYTES as u64 {
        return Err(format!(
            "messages/{message_id}.json: CodeBuddy message JSON exceeds max bytes ({MAX_PROVIDER_JSONL_LINE_BYTES})"
        ));
    }
    Ok((path, file))
}

pub(super) fn codebuddy_extension_line_number(
    session_ordinal: usize,
    message_index: usize,
) -> usize {
    session_ordinal
        .saturating_mul(10_000)
        .saturating_add(message_index)
        .saturating_add(1)
}

pub(super) fn codebuddy_extension_metadata_text(
    metadata: &CodeBuddyExtensionMetadata,
    fields: &[&str],
) -> Option<String> {
    metadata
        .conversation
        .as_ref()
        .and_then(|value| task_json_string_field(value, fields))
        .filter(|value| value.len() <= CODEBUDDY_MAX_CHECKPOINT_TEXT_BYTES)
}

fn codebuddy_project_index_and_conversation(
    project_dir: &Path,
    native_session_id: &str,
    summary: &mut ProviderImportSummary,
    line: usize,
) -> (Option<Value>, Option<Value>) {
    let path = project_dir.join("index.json");
    let value = match fs::symlink_metadata(&path) {
        Ok(_) => match ensure_regular_provider_transcript_file(&path).and_then(|_| {
            read_json_file_limited(
                &path,
                MAX_PROVIDER_JSONL_LINE_BYTES,
                "CodeBuddy project index.json",
            )
        }) {
            Ok(value) => value,
            Err(err) => {
                summary.record_failure(ProviderImportFailure {
                    line,
                    error: format!("project index.json: {err}"),
                });
                return (None, None);
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (None, None),
        Err(err) => {
            summary.record_failure(ProviderImportFailure {
                line,
                error: format!("project index.json: {err}"),
            });
            return (None, None);
        }
    };
    let conversation = value
        .get("conversations")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(native_session_id))
        })
        .cloned();
    (Some(value), conversation)
}

pub(super) fn codebuddy_message_time(
    raw_message: &Value,
    decoded_message: &Value,
    message_path: &Path,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    task_json_time_field(
        raw_message,
        &["createdAt", "created_at", "timestamp", "time", "date"],
    )
    .or_else(|| {
        task_json_time_field(
            decoded_message,
            &["createdAt", "created_at", "timestamp", "time", "date"],
        )
    })
    .or_else(|| {
        fs::metadata(message_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(DateTime::<Utc>::from)
    })
    .unwrap_or(fallback)
}

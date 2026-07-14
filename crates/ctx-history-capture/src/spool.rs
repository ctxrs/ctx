use std::{
    collections::BinaryHeap,
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    inbox_dir as core_inbox_dir, new_id, utc_now, CaptureEnvelope, HistoryRecord,
    SessionHistoryArchive,
};
use ctx_history_store::Store;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::summaries::ArchiveCounts;

use crate::common::io::{read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead};
use crate::common::json::payload_has_record_fields;
use crate::{
    sanitize_filename_component, stable_capture_uuid, CaptureError, Result, SpoolCounts,
    SpoolImportFailure, SpoolImportSummary, SpoolRepairSummary, CAPTURE_SCHEMA_VERSION,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SPOOL_DIRECTORY_BATCH_FILES: usize = 64;

#[derive(Debug)]
pub struct SpoolWriter {
    tmp_path: PathBuf,
    final_path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl SpoolWriter {
    pub fn create(inbox: impl AsRef<Path>, machine_id: &str) -> Result<Self> {
        let inbox = inbox.as_ref();
        create_private_spool_dir(inbox)?;

        let machine_id = sanitize_filename_component(machine_id);
        let pid = std::process::id();
        let unix_ms = utc_now().timestamp_millis();
        let random = new_id().simple().to_string();
        let name = format!("capture-{machine_id}-{pid}-{unix_ms}-{random}.jsonl");
        let final_path = inbox.join(name);
        let tmp_path = append_suffix(&final_path, ".tmp")?;
        let file = open_private_spool_file(&tmp_path, true)?;

        Ok(Self {
            tmp_path,
            final_path,
            writer: Some(BufWriter::new(file)),
        })
    }

    pub fn tmp_path(&self) -> &Path {
        &self.tmp_path
    }

    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    pub fn write_envelope(&mut self, envelope: &CaptureEnvelope) -> Result<()> {
        let writer = self.writer.as_mut().ok_or(CaptureError::WriterClosed)?;
        serde_json::to_writer(&mut *writer, envelope)?;
        writer.write_all(b"\n")?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        let mut writer = self.writer.take().ok_or(CaptureError::WriterClosed)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&self.tmp_path, &self.final_path)?;
        Ok(self.final_path)
    }
}

pub fn inbox_dir(data_root: impl AsRef<Path>) -> PathBuf {
    core_inbox_dir(data_root.as_ref().to_path_buf())
}

pub fn read_jsonl(path: impl AsRef<Path>) -> Result<Vec<CaptureEnvelope>> {
    let path = path.as_ref();
    let mut envelopes = Vec::new();
    stream_jsonl(path, |envelope| {
        envelopes.push(envelope);
        Ok(())
    })?;
    Ok(envelopes)
}

pub fn import_spool(inbox: impl AsRef<Path>, store: &mut Store) -> Result<SpoolImportSummary> {
    let inbox = inbox.as_ref();
    create_private_spool_dir(inbox)?;
    let mut summary = SpoolImportSummary::default();
    // Recover already-claimed files first. Pending files claimed during this
    // invocation are therefore never immediately replayed a second time.
    for processing_phase in [true, false] {
        let mut after = None;
        loop {
            let files = importable_spool_file_batch(inbox, processing_phase, after.as_deref())?;
            if files.is_empty() {
                break;
            }
            for candidate in files {
                after = Some(candidate.clone());
                import_spool_candidate(candidate, store, &mut summary)?;
            }
        }
    }

    Ok(summary)
}

pub fn spool_counts(inbox: impl AsRef<Path>) -> Result<SpoolCounts> {
    let inbox = inbox.as_ref();
    let mut counts = SpoolCounts::default();
    if !inbox.exists() {
        return Ok(counts);
    }

    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.ends_with(".jsonl") {
            counts.pending += 1;
        } else if file_name.ends_with(".jsonl.tmp") {
            counts.tmp += 1;
        } else if file_name.ends_with(".jsonl.processing") {
            counts.processing += 1;
        } else if file_name.ends_with(".jsonl.done") {
            counts.done += 1;
        } else if file_name.ends_with(".jsonl.failed") {
            counts.failed += 1;
        }
    }

    Ok(counts)
}

pub fn retry_failed_spool_files(inbox: impl AsRef<Path>) -> Result<SpoolRepairSummary> {
    let inbox = inbox.as_ref();
    create_private_spool_dir(inbox)?;
    let mut summary = SpoolRepairSummary::default();

    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let failed_path = entry.path();
        let file_name = failed_path
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let Some(pending_name) = file_name.strip_suffix(".failed") else {
            continue;
        };
        if !pending_name.ends_with(".jsonl") {
            continue;
        }
        let pending_path = failed_path.with_file_name(pending_name);
        if pending_path.exists() {
            return Err(CaptureError::InvalidPath(pending_path));
        }
        let sidecar = append_suffix(&failed_path, ".error.json")?;
        fs::rename(&failed_path, &pending_path)?;
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
        summary.retried_files += 1;
    }

    Ok(summary)
}

pub fn archive_from_envelopes(envelopes: &[CaptureEnvelope]) -> Result<SessionHistoryArchive> {
    let mut archive = SessionHistoryArchive::default();

    for envelope in envelopes {
        validate_envelope(envelope)?;
        if let Some(archive_value) = envelope.payload.get("archive") {
            let nested: SessionHistoryArchive = serde_json::from_value(archive_value.clone())?;
            archive.records.extend(nested.records);
            archive.capture_sources.extend(nested.capture_sources);
            archive.sessions.extend(nested.sessions);
            archive.runs.extend(nested.runs);
            archive.events.extend(nested.events);
            archive.artifact_records.extend(nested.artifact_records);
            archive.vcs_workspaces.extend(nested.vcs_workspaces);
            archive.vcs_changes.extend(nested.vcs_changes);
            archive
                .history_record_links
                .extend(nested.history_record_links);
            archive.summaries.extend(nested.summaries);
            archive.files_touched.extend(nested.files_touched);
            continue;
        }

        let record_value = envelope
            .payload
            .get("record")
            .filter(|value| value.is_object());
        let should_create_record =
            record_value.is_some() || payload_has_record_fields(&envelope.payload);

        if should_create_record {
            let value = record_value.unwrap_or(&envelope.payload);
            let record = record_from_envelope(envelope, value)?;
            archive.records.push(record);
        }
    }

    Ok(archive)
}

fn import_processing_file(path: &Path, store: &mut Store) -> Result<ArchiveCounts> {
    let mut counts = ArchiveCounts::default();
    stream_jsonl(path, |envelope| {
        counts.add(import_envelope(store, &envelope)?);
        Ok(())
    })?;
    Ok(counts)
}

fn stream_jsonl(path: &Path, mut consume: impl FnMut(CaptureEnvelope) -> Result<()>) -> Result<()> {
    ensure_regular_spool_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut line_number = 0usize;
    loop {
        let read = read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)?;
        if read == ProviderJsonlLineRead::Eof {
            break;
        }
        line_number += 1;
        if matches!(read, ProviderJsonlLineRead::Oversized { .. }) {
            return Err(CaptureError::SpoolEnvelopeTooLarge {
                path: path.to_path_buf(),
                line: line_number,
                max_bytes: MAX_PROVIDER_JSONL_LINE_BYTES,
            });
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let envelope =
            serde_json::from_slice(&line).map_err(|source| CaptureError::InvalidJsonLine {
                path: path.to_path_buf(),
                line: line_number,
                source,
            })?;
        validate_envelope(&envelope)?;
        consume(envelope)?;
    }
    Ok(())
}

fn import_envelope(store: &mut Store, envelope: &CaptureEnvelope) -> Result<ArchiveCounts> {
    let archive = archive_from_envelopes(std::slice::from_ref(envelope))?;
    let source_id = stable_capture_uuid(&envelope.dedupe_key, "source");
    let outcome = store.import_archive_from_capture_source(
        &archive,
        source_id,
        &envelope.source,
        envelope.occurred_at,
        envelope.fidelity,
        true,
    )?;
    Ok(ArchiveCounts {
        records: archive.records.len(),
        maintenance_pending: !outcome.search_projection_ready,
        maintenance_errors: outcome.maintenance_error.into_iter().collect(),
    })
}

fn validate_envelope(envelope: &CaptureEnvelope) -> Result<()> {
    if envelope.schema_version == CAPTURE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(CaptureError::UnsupportedSchemaVersion(
            envelope.schema_version,
        ))
    }
}

fn importable_spool_file_batch(
    inbox: &Path,
    processing: bool,
    after: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let mut files = BinaryHeap::new();
    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let path = entry.path();
        let matches_phase = if processing {
            is_processing_spool_file(&path)
        } else {
            is_pending_spool_file(&path)
        };
        if !matches_phase || after.is_some_and(|after| path.as_path() <= after) {
            continue;
        }
        ensure_regular_spool_file(&path)?;
        files.push(path);
        if files.len() > SPOOL_DIRECTORY_BATCH_FILES {
            files.pop();
        }
    }
    let mut files = files.into_vec();
    files.sort_unstable();
    Ok(files)
}

fn import_spool_candidate(
    candidate: PathBuf,
    store: &mut Store,
    summary: &mut SpoolImportSummary,
) -> Result<()> {
    let processing = if is_processing_spool_file(&candidate) {
        candidate
    } else {
        match claim_pending_file(&candidate) {
            Ok(path) => path,
            Err(CaptureError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                summary.skipped_files += 1;
                return Ok(());
            }
            Err(err) => return Err(err),
        }
    };

    match import_processing_file(&processing, store) {
        Ok(counts) => {
            let done = state_path(&processing, ".done")?;
            summary.processed_files += 1;
            summary.imported_records += counts.records;
            summary.maintenance_pending_files += usize::from(counts.maintenance_pending);
            summary
                .maintenance_failures
                .extend(
                    counts
                        .maintenance_errors
                        .into_iter()
                        .map(|error| SpoolImportFailure {
                            path: done.clone(),
                            error,
                        }),
                );
            if let Err(error) = finalize_processing_file(&processing, &done) {
                summary.finalization_warnings.push(SpoolImportFailure {
                    path: processing,
                    error: format!(
                        "capture committed durably but spool finalization was deferred: {error}"
                    ),
                });
            }
        }
        Err(err) => {
            let failed = state_path(&processing, ".failed")?;
            fs::rename(&processing, &failed)?;
            write_failure_metadata(&failed, &err)?;
            summary.processed_files += 1;
            summary.failed_files += 1;
            summary.failures.push(SpoolImportFailure {
                path: failed,
                error: err.to_string(),
            });
        }
    }
    Ok(())
}

fn is_pending_spool_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".jsonl"))
}

fn is_processing_spool_file(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".jsonl.processing"))
}

fn finalize_processing_file(processing: &Path, done: &Path) -> Result<()> {
    match fs::rename(processing, done) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && done.is_file() => {
            fs::remove_file(processing)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn claim_pending_file(path: &Path) -> Result<PathBuf> {
    ensure_regular_spool_file(path)?;
    let processing = append_suffix(path, ".processing")?;
    fs::rename(path, &processing)?;
    Ok(processing)
}

fn ensure_regular_spool_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        restrict_private_spool_file(path)?;
        Ok(())
    } else {
        Err(CaptureError::InvalidPath(path.to_path_buf()))
    }
}

fn write_failure_metadata(failed_path: &Path, err: &CaptureError) -> Result<()> {
    let sidecar = append_suffix(failed_path, ".error.json")?;
    let metadata = json!({
        "failed_at": utc_now(),
        "spool_file": failed_path,
        "error": err.to_string(),
    });
    let mut file = open_private_spool_file(&sidecar, false)?;
    file.write_all(&serde_json::to_vec_pretty(&metadata)?)?;
    file.sync_all()?;
    Ok(())
}

fn create_private_spool_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    Ok(())
}

fn open_private_spool_file(path: &Path, create_new: bool) -> Result<File> {
    let mut options = File::options();
    options
        .write(true)
        .create_new(create_new)
        .create(!create_new)
        .truncate(!create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    restrict_private_spool_file(path)?;
    Ok(file)
}

fn restrict_private_spool_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| CaptureError::InvalidPath(path.to_path_buf()))?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{file_name}{suffix}")))
}

fn state_path(processing_path: &Path, state_suffix: &str) -> Result<PathBuf> {
    let file_name = processing_path
        .file_name()
        .ok_or_else(|| CaptureError::InvalidPath(processing_path.to_path_buf()))?
        .to_string_lossy();
    let base = file_name
        .strip_suffix(".processing")
        .ok_or_else(|| CaptureError::InvalidPath(processing_path.to_path_buf()))?;
    Ok(processing_path.with_file_name(format!("{base}{state_suffix}")))
}

fn record_from_envelope(envelope: &CaptureEnvelope, value: &Value) -> Result<HistoryRecord> {
    let id = uuid_field(value, "id")?
        .unwrap_or_else(|| stable_capture_uuid(&envelope.dedupe_key, "record"));
    let title = string_field(value, "title")
        .or_else(|| string_field(value, "summary"))
        .unwrap_or_else(|| format!("Captured {} event", envelope.source.provider));
    let body = match string_field(value, "body").or_else(|| string_field(value, "summary")) {
        Some(body) => body,
        None => serde_json::to_string_pretty(&envelope.payload)?,
    };
    let tags = string_array_field(value, "tags")?.unwrap_or_else(|| {
        vec![
            "capture".to_owned(),
            envelope.source.provider.as_str().to_owned(),
        ]
    });
    let kind = string_field(value, "record_kind")
        .or_else(|| string_field(value, "history_record_kind"))
        .or_else(|| string_field(value, "kind").filter(|kind| kind != "history_record"))
        .unwrap_or_else(|| "capture".to_owned());
    let workspace = string_field(value, "workspace")
        .or_else(|| envelope.cwd.clone())
        .or_else(|| envelope.source.cwd.clone());
    let created_at = datetime_field(value, "created_at")?.unwrap_or(envelope.occurred_at);
    let updated_at = datetime_field(value, "updated_at")?.unwrap_or(created_at);

    Ok(HistoryRecord {
        id,
        title,
        body,
        tags,
        kind,
        workspace,
        created_at,
        updated_at,
    })
}

fn uuid_field(value: &Value, field: &str) -> Result<Option<Uuid>> {
    match value.get(field) {
        Some(Value::String(raw)) => Ok(Some(Uuid::parse_str(raw)?)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be a UUID string"
        ))),
    }
}

fn datetime_field(value: &Value, field: &str) -> Result<Option<DateTime<Utc>>> {
    match value.get(field) {
        Some(Value::String(raw)) => {
            Ok(Some(DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc)))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be an RFC3339 timestamp string"
        ))),
    }
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn string_array_field(value: &Value, field: &str) -> Result<Option<Vec<String>>> {
    match value.get(field) {
        Some(Value::Array(items)) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                let item = item.as_str().ok_or_else(|| {
                    CaptureError::InvalidPayload(format!("{field} must contain only strings"))
                })?;
                values.push(item.to_owned());
            }
            Ok(Some(values))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be an array of strings"
        ))),
    }
}

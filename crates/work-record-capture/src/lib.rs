use std::{
    env,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;
use work_record_core::{
    inbox_dir as core_inbox_dir, new_id, CaptureEnvelope, CaptureProvider, CaptureSourceDescriptor,
    CaptureSourceKind, Evidence, Fidelity, WorkRecord, WorkRecordArchive,
};
use work_record_store::Store;

pub const CAPTURE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Store(#[from] work_record_store::StoreError),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("unsupported capture envelope schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid spool path: {0:?}")]
    InvalidPath(PathBuf),
    #[error("spool writer is already closed")]
    WriterClosed,
    #[error("line {line} in {path:?} is not a valid capture envelope: {source}")]
    InvalidJsonLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, CaptureError>;

#[derive(Debug)]
pub struct SpoolWriter {
    tmp_path: PathBuf,
    final_path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl SpoolWriter {
    pub fn create(inbox: impl AsRef<Path>, machine_id: &str) -> Result<Self> {
        let inbox = inbox.as_ref();
        fs::create_dir_all(inbox)?;

        let machine_id = sanitize_filename_component(machine_id);
        let pid = std::process::id();
        let unix_ms = Utc::now().timestamp_millis();
        let random = new_id().simple().to_string();
        let name = format!("capture-{machine_id}-{pid}-{unix_ms}-{random}.jsonl");
        let final_path = inbox.join(name);
        let tmp_path = append_suffix(&final_path, ".tmp")?;
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;

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

#[derive(Debug, Clone)]
pub struct FixtureOptions {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub dedupe_key: Option<String>,
    pub machine_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub occurred_at: DateTime<Utc>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            title: "Fixture capture".to_owned(),
            body: "fixture body".to_owned(),
            tags: vec!["fixture".to_owned()],
            dedupe_key: None,
            machine_id: None,
            cwd: None,
            occurred_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolCounts {
    pub pending: usize,
    pub tmp: usize,
    pub processing: usize,
    pub done: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolImportFailure {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolImportSummary {
    pub processed_files: usize,
    pub skipped_files: usize,
    pub imported_records: usize,
    pub imported_evidence: usize,
    pub failed_files: usize,
    pub failures: Vec<SpoolImportFailure>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ArchiveCounts {
    records: usize,
    evidence: usize,
}

pub fn inbox_dir(data_root: impl AsRef<Path>) -> PathBuf {
    core_inbox_dir(data_root.as_ref().to_path_buf())
}

pub fn write_fixture(inbox: impl AsRef<Path>, options: FixtureOptions) -> Result<PathBuf> {
    let envelope = fixture_envelope(options)?;
    let mut writer = SpoolWriter::create(inbox, &envelope.source.machine_id)?;
    writer.write_envelope(&envelope)?;
    writer.finish()
}

pub fn fixture_envelope(options: FixtureOptions) -> Result<CaptureEnvelope> {
    let machine_id = options.machine_id.unwrap_or_else(default_machine_id);
    let cwd_path = match options.cwd {
        Some(path) => path,
        None => env::current_dir()?,
    };
    let cwd = cwd_path.display().to_string();
    let dedupe_key = options
        .dedupe_key
        .unwrap_or_else(|| format!("fixture:{}", new_id()));
    let tags = if options.tags.is_empty() {
        vec!["fixture".to_owned()]
    } else {
        options.tags
    };
    let payload = json!({
        "kind": "work_record",
        "title": options.title,
        "body": options.body,
        "tags": tags,
        "record_kind": "capture-fixture",
        "workspace": cwd,
    });
    let payload_hash = Some(compute_payload_hash(&payload)?);

    Ok(CaptureEnvelope {
        schema_version: CAPTURE_SCHEMA_VERSION,
        capture_event_id: new_id(),
        dedupe_key,
        source: CaptureSourceDescriptor {
            kind: CaptureSourceKind::DirectCli,
            provider: CaptureProvider::Unknown,
            machine_id,
            process_id: Some(std::process::id()),
            cwd: Some(cwd.clone()),
            raw_source_path: None,
            external_session_id: None,
        },
        occurred_at: options.occurred_at,
        cwd: Some(cwd),
        env_session_hints: json!({}),
        payload,
        payload_hash,
        fidelity: Fidelity::Imported,
    })
}

pub fn read_jsonl(path: impl AsRef<Path>) -> Result<Vec<CaptureEnvelope>> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut envelopes = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope: CaptureEnvelope =
            serde_json::from_str(&line).map_err(|source| CaptureError::InvalidJsonLine {
                path: path.to_path_buf(),
                line: index + 1,
                source,
            })?;
        validate_envelope(&envelope)?;
        envelopes.push(envelope);
    }

    Ok(envelopes)
}

pub fn import_spool(inbox: impl AsRef<Path>, store: &mut Store) -> Result<SpoolImportSummary> {
    let inbox = inbox.as_ref();
    fs::create_dir_all(inbox)?;
    let mut summary = SpoolImportSummary::default();
    let files = pending_spool_files(inbox)?;

    for pending in files {
        let processing = match claim_pending_file(&pending) {
            Ok(path) => path,
            Err(CaptureError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                summary.skipped_files += 1;
                continue;
            }
            Err(err) => return Err(err),
        };

        match import_processing_file(&processing, store) {
            Ok(counts) => {
                let done = state_path(&processing, ".done")?;
                fs::rename(&processing, done)?;
                summary.processed_files += 1;
                summary.imported_records += counts.records;
                summary.imported_evidence += counts.evidence;
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

pub fn archive_from_envelopes(envelopes: &[CaptureEnvelope]) -> Result<WorkRecordArchive> {
    let mut archive = WorkRecordArchive {
        version: 1,
        records: Vec::new(),
        evidence: Vec::new(),
    };

    for envelope in envelopes {
        validate_envelope(envelope)?;
        if let Some(archive_value) = envelope.payload.get("archive") {
            let nested: WorkRecordArchive = serde_json::from_value(archive_value.clone())?;
            archive.records.extend(nested.records);
            archive.evidence.extend(nested.evidence);
            continue;
        }

        let evidence_value = envelope
            .payload
            .get("evidence")
            .filter(|value| value.is_object());
        let record_value = envelope
            .payload
            .get("record")
            .filter(|value| value.is_object());
        let should_create_record = record_value.is_some()
            || payload_has_record_fields(&envelope.payload)
            || evidence_value.is_none();

        let record_id = if should_create_record {
            let value = record_value.unwrap_or(&envelope.payload);
            let record = record_from_envelope(envelope, value)?;
            let id = record.id;
            archive.records.push(record);
            Some(id)
        } else {
            None
        };

        if let Some(value) = evidence_value {
            archive
                .evidence
                .push(evidence_from_envelope(envelope, value, record_id)?);
        } else if payload_has_evidence_fields(&envelope.payload) {
            archive.evidence.push(evidence_from_envelope(
                envelope,
                &envelope.payload,
                record_id,
            )?);
        }
    }

    Ok(archive)
}

pub fn stable_capture_uuid(dedupe_key: &str, role: &str) -> Uuid {
    let mut bytes = [0_u8; 16];
    let name = format!("ctx-work-record-capture:{dedupe_key}:{role}");
    let first = fnv1a64(name.as_bytes()).to_be_bytes();
    let second = fnv1a64(format!("{name}:uuid-v7").as_bytes()).to_be_bytes();

    bytes[..6].copy_from_slice(&first[..6]);
    bytes[6] = 0x70 | (first[6] & 0x0f);
    bytes[7] = first[7];
    bytes[8] = 0x80 | (second[0] & 0x3f);
    bytes[9..].copy_from_slice(&second[1..]);
    Uuid::from_bytes(bytes)
}

pub fn compute_payload_hash(payload: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(payload)?;
    Ok(format!("fnv1a64:{:016x}", fnv1a64(&bytes)))
}

fn import_processing_file(path: &Path, store: &mut Store) -> Result<ArchiveCounts> {
    let envelopes = read_jsonl(path)?;
    let archive = archive_from_envelopes(&envelopes)?;
    let counts = ArchiveCounts {
        records: archive.records.len(),
        evidence: archive.evidence.len(),
    };
    store.import_archive(&archive, true)?;
    Ok(counts)
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

fn pending_spool_files(inbox: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .map(|name| name.to_string_lossy().ends_with(".jsonl"))
            .unwrap_or(false)
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn claim_pending_file(path: &Path) -> Result<PathBuf> {
    let processing = append_suffix(path, ".processing")?;
    fs::rename(path, &processing)?;
    Ok(processing)
}

fn write_failure_metadata(failed_path: &Path, err: &CaptureError) -> Result<()> {
    let sidecar = append_suffix(failed_path, ".error.json")?;
    let metadata = json!({
        "failed_at": Utc::now(),
        "spool_file": failed_path,
        "error": err.to_string(),
    });
    fs::write(sidecar, serde_json::to_vec_pretty(&metadata)?)?;
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

fn record_from_envelope(envelope: &CaptureEnvelope, value: &Value) -> Result<WorkRecord> {
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
        .or_else(|| string_field(value, "work_record_kind"))
        .or_else(|| {
            string_field(value, "kind").filter(|kind| kind != "work_record" && kind != "evidence")
        })
        .unwrap_or_else(|| "capture".to_owned());
    let workspace = string_field(value, "workspace")
        .or_else(|| envelope.cwd.clone())
        .or_else(|| envelope.source.cwd.clone());
    let created_at = datetime_field(value, "created_at")?.unwrap_or(envelope.occurred_at);
    let updated_at = datetime_field(value, "updated_at")?.unwrap_or(created_at);

    Ok(WorkRecord {
        id,
        title,
        body,
        tags,
        kind,
        workspace,
        pr_url: string_field(value, "pr_url"),
        created_at,
        updated_at,
    })
}

fn evidence_from_envelope(
    envelope: &CaptureEnvelope,
    value: &Value,
    default_record_id: Option<Uuid>,
) -> Result<Evidence> {
    let id = uuid_field(value, "id")?
        .unwrap_or_else(|| stable_capture_uuid(&envelope.dedupe_key, "evidence"));
    let record_id = uuid_field(value, "record_id")?.or(default_record_id);
    let command = string_field(value, "command")
        .unwrap_or_else(|| format!("captured {} event", envelope.source.provider));
    let exit_code = i64_field(value, "exit_code")?
        .map(|value| value as i32)
        .unwrap_or(0);
    let stdout = string_field(value, "stdout").unwrap_or_default();
    let stderr = string_field(value, "stderr").unwrap_or_default();
    let started_at = datetime_field(value, "started_at")?.unwrap_or(envelope.occurred_at);
    let duration_ms = i64_field(value, "duration_ms")?.unwrap_or(0);

    Ok(Evidence {
        id,
        record_id,
        command,
        exit_code,
        stdout,
        stderr,
        started_at,
        duration_ms,
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

fn i64_field(value: &Value, field: &str) -> Result<Option<i64>> {
    match value.get(field) {
        Some(Value::Number(number)) => number.as_i64().map(Some).ok_or_else(|| {
            CaptureError::InvalidPayload(format!("{field} must be a signed integer"))
        }),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be a signed integer"
        ))),
    }
}

fn payload_has_record_fields(value: &Value) -> bool {
    [
        "title",
        "body",
        "summary",
        "tags",
        "record_kind",
        "work_record_kind",
        "workspace",
        "pr_url",
    ]
    .iter()
    .any(|field| value.get(*field).is_some())
}

fn payload_has_evidence_fields(value: &Value) -> bool {
    [
        "command",
        "exit_code",
        "stdout",
        "stderr",
        "started_at",
        "duration_ms",
        "record_id",
    ]
    .iter()
    .any(|field| value.get(*field).is_some())
}

fn default_machine_id() -> String {
    env::var("CTX_MACHINE_ID")
        .or_else(|_| env::var("HOSTNAME"))
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "local".to_owned())
}

fn sanitize_filename_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        let root = std::env::current_dir().unwrap().join("target/test-data");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("work-record-capture-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixture_options(dedupe_key: &str, title: &str) -> FixtureOptions {
        FixtureOptions {
            title: title.to_owned(),
            body: "captured body".to_owned(),
            tags: vec!["capture-test".to_owned()],
            dedupe_key: Some(dedupe_key.to_owned()),
            machine_id: Some("test-machine".to_owned()),
            cwd: Some(PathBuf::from("/tmp/work")),
            occurred_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    #[test]
    fn spool_writer_closes_tmp_file_atomically_to_jsonl() {
        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        let envelope = fixture_envelope(fixture_options("atomic", "Atomic capture")).unwrap();
        let mut writer = SpoolWriter::create(&inbox, "test-machine").unwrap();
        let tmp_path = writer.tmp_path().to_path_buf();
        let final_path = writer.final_path().to_path_buf();

        writer.write_envelope(&envelope).unwrap();
        assert!(tmp_path.exists());
        assert!(!final_path.exists());

        let closed_path = writer.finish().unwrap();
        assert_eq!(closed_path, final_path);
        assert!(!tmp_path.exists());
        assert!(final_path.exists());
        assert_eq!(read_jsonl(&final_path).unwrap(), vec![envelope]);
    }

    #[test]
    fn failed_import_retains_raw_failed_file_and_error_metadata() {
        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let pending = inbox.join("capture-bad.jsonl");
        fs::write(&pending, "not json\n").unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_spool(&inbox, &mut store).unwrap();

        assert_eq!(summary.failed_files, 1);
        assert_eq!(summary.processed_files, 1);
        let failed = inbox.join("capture-bad.jsonl.failed");
        let sidecar = inbox.join("capture-bad.jsonl.failed.error.json");
        assert!(failed.exists());
        assert!(sidecar.exists());
        assert_eq!(fs::read_to_string(failed).unwrap(), "not json\n");
        assert!(fs::read_to_string(sidecar)
            .unwrap()
            .contains("not a valid capture envelope"));
        assert_eq!(spool_counts(&inbox).unwrap().failed, 1);
    }

    #[test]
    fn import_is_idempotent_by_dedupe_key() {
        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        let envelope = fixture_envelope(fixture_options("same-dedupe", "First title")).unwrap();
        let mut first = SpoolWriter::create(&inbox, "test-machine").unwrap();
        first.write_envelope(&envelope).unwrap();
        first.finish().unwrap();
        let mut second = SpoolWriter::create(&inbox, "test-machine").unwrap();
        second.write_envelope(&envelope).unwrap();
        second.finish().unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_spool(&inbox, &mut store).unwrap();

        assert_eq!(summary.failed_files, 0);
        assert_eq!(summary.processed_files, 2);
        let records = store.list_records(10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, stable_capture_uuid("same-dedupe", "record"));
        assert_eq!(records[0].id.get_version_num(), 7);
        assert_eq!(records[0].title, "First title");
        assert_eq!(spool_counts(&inbox).unwrap().done, 2);
    }
}

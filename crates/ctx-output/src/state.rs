// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Local-only settings and measured usage. Unix (including macOS) uses XDG paths.
//! Metrics rotate at 10 MiB, retaining one backup; the next rotation replaces it.
//! Original retention defaults to 100 entries / 100 MiB / 30 days, configurable locally.
//! No arguments, raw output, estimates, network calls, or history scans by default.
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const METRICS_LIMIT: u64 = 10 * 1024 * 1024;
const ORIGINAL_LIMIT: u64 = 100 * 1024 * 1024;
const ORIGINAL_AGE: u64 = 30;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

mod commands;
mod settings;

pub(crate) use commands::*;
pub(crate) use settings::SemanticMode;
pub(crate) use settings::{Settings, config_path, state_dir};

#[derive(Clone, Debug, Serialize, Deserialize)]
// Stored records may add metadata without changing Event literal callers.
pub struct Event {
    pub unix_millis: u64,
    /// Basename or category only, never a command line.
    pub command: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub source: Option<String>,
    pub original_id: Option<String>,
}
/// A persisted event with optional project identity. Legacy records have no project.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedEvent {
    #[serde(flatten)]
    pub event: Event,
    #[serde(default)]
    pub project: Option<String>,
}
impl std::ops::Deref for RecordedEvent {
    type Target = Event;
    fn deref(&self) -> &Event {
        &self.event
    }
}
/// Canonical checkout root (including linked worktrees), or the directory itself
/// outside Git. No subprocesses or repository metadata writes are involved.
pub fn project_at(path: &Path) -> Result<String> {
    let path = fs::canonicalize(path).context("project directory is unavailable")?;
    ensure!(path.is_dir(), "project must be a directory");
    let root = path
        .ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(&path);
    Ok(root
        .to_str()
        .context("project path must be UTF-8")?
        .to_owned())
}
fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn validate(event: &Event) -> Result<()> {
    ensure!(
        valid_label(&event.command) && event.source.as_deref().is_none_or(valid_label),
        "command and source must be short basenames/categories, without arguments"
    );
    ensure!(
        event.original_id.as_deref().is_none_or(valid_id),
        "invalid original ID"
    );
    Ok(())
}
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
/// record_usage governs all storage; enabled remains a caller output decision.
/// Originals are independently gated by the persisted keep_originals setting.
/// Recording is best effort: a busy state lock skips this event and its originals.
pub fn record(event: Event, originals: Option<(&[u8], &[u8])>) -> Result<()> {
    let settings = Settings::load()?;
    if !settings.record_usage {
        return Ok(());
    }
    let project = std::env::current_dir()
        .ok()
        .and_then(|p| project_at(&p).ok());
    match project {
        Some(project) => {
            record_project_at(&state_dir()?, &settings, event, originals, Some(&project))
        }
        None => record_at(&state_dir()?, &settings, event, originals),
    }
}

/// Save a full semantic input without changing usage totals. This deliberately
/// ignores record_usage and keep_originals; semantic omission is recoverable or
/// it is not emitted.
pub fn save_semantic_original(bytes: &[u8]) -> Result<Option<String>> {
    save_semantic_original_at(&state_dir()?, &Settings::load()?, bytes)
}

pub fn save_semantic_original_at(
    dir: &Path,
    settings: &Settings,
    bytes: &[u8],
) -> Result<Option<String>> {
    settings.validate()?;
    if bytes.len() as u64 > settings.originals_max_bytes.min(ORIGINAL_LIMIT) {
        return Ok(None);
    }
    let Some(_lock) = try_lock(dir)? else {
        return Ok(None);
    };
    let originals_dir = dir.join("originals");
    private_dir(&originals_dir)?;
    let id = save_original(&originals_dir, bytes, &[])?;
    evict_originals(&originals_dir, &id, settings)?;
    Ok(Some(id))
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct SemanticUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct SemanticReceipt {
    pub unix_millis: u64,
    pub status: &'static str,
    pub disposition: &'static str,
    pub model: &'static str,
    pub http_status: Option<u16>,
    pub usage: Option<SemanticUsage>,
    pub latency_ms: u64,
    pub passage_count: usize,
    pub selected_count: usize,
    pub omitted_count: usize,
    pub ordinary_tokens: usize,
    pub semantic_tokens: Option<usize>,
    pub memoized: bool,
}

pub fn record_semantic_receipt(receipt: &SemanticReceipt) -> Result<()> {
    record_semantic_receipt_at(&state_dir()?, receipt)
}

pub fn record_semantic_receipt_at(dir: &Path, receipt: &SemanticReceipt) -> Result<()> {
    ensure!(
        receipt.model == "jev-1.13.0",
        "invalid semantic receipt model"
    );
    let Some(_lock) = try_lock(dir)? else {
        return Ok(());
    };
    let mut bytes = serde_json::to_vec(receipt)?;
    bytes.push(b'\n');
    append_jsonl(
        dir,
        "semantic.jsonl",
        "semantic.1.jsonl",
        "semantic receipts",
        &bytes,
    )
}

fn append_jsonl(
    dir: &Path,
    name: &str,
    backup_name: &str,
    label: &str,
    bytes: &[u8],
) -> Result<()> {
    let path = dir.join(name);
    let size = match fs::symlink_metadata(&path) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "{label} must be a regular file"
            );
            meta.len()
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    // Preserve an interrupted append as a malformed record rather than joining
    // the next valid event onto it. Never rewrite or discard the damaged bytes.
    let needs_separator = if size > 0 {
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        last[0] != b'\n'
    } else {
        false
    };
    let rotate = size + bytes.len() as u64 + u64::from(needs_separator) > METRICS_LIMIT;
    if rotate {
        let backup = dir.join(backup_name);
        remove_if_exists(&backup)?;
        fs::rename(&path, backup)?;
    }
    let mut file = private_open(&path, true, false)?;
    if needs_separator && !rotate {
        file.write_all(b"\n")?;
    }
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}
pub fn record_at(
    dir: &Path,
    settings: &Settings,
    event: Event,
    originals: Option<(&[u8], &[u8])>,
) -> Result<()> {
    record_project_at(dir, settings, event, originals, None)
}
/// Explicit project identity for callers whose command ran outside Sift's cwd.
/// Use project_at to normalize an existing directory before recording/querying.
pub fn record_project_at(
    dir: &Path,
    settings: &Settings,
    mut event: Event,
    originals: Option<(&[u8], &[u8])>,
    project: Option<&str>,
) -> Result<()> {
    if !settings.record_usage {
        return Ok(());
    }
    settings.validate()?;
    ensure!(
        project.is_none_or(|p| !p.is_empty() && p.len() <= 4096 && !p.contains('\0')),
        "invalid project identity"
    );
    validate(&event)?;
    let Some(_lock) = try_lock(dir)? else {
        return Ok(());
    };
    // IDs are assigned here only after both streams have been saved successfully.
    event.original_id = None;
    if settings.keep_originals
        && let Some((stdout, stderr)) = originals
        && stdout.len() as u64 + stderr.len() as u64
            <= settings.originals_max_bytes.min(ORIGINAL_LIMIT)
    {
        // An oversized original is not stored, but its measured event still is.
        let originals_dir = dir.join("originals");
        private_dir(&originals_dir)?;
        let id = save_original(&originals_dir, stdout, stderr)?;
        evict_originals(&originals_dir, &id, settings)?;
        event.original_id = Some(id);
    }
    let mut bytes = serde_json::to_vec(&RecordedEvent {
        event,
        project: project.map(str::to_owned),
    })?;
    bytes.push(b'\n');
    append_jsonl(dir, "metrics.jsonl", "metrics.1.jsonl", "metrics", &bytes)
}
fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "ctx output directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn private_open(path: &Path, append: bool, new: bool) -> Result<File> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "ctx output state path must be a regular file"
        );
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(!new)
        .create_new(new)
        .append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
fn lock(dir: &Path) -> Result<File> {
    private_dir(dir)?;
    let file = private_open(&dir.join("state.lock"), false, false)?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}
fn try_lock(dir: &Path) -> Result<Option<File>> {
    private_dir(dir)?;
    let file = private_open(&dir.join("state.lock"), false, false)?;
    // Brief contention (including descriptors inherited across fork/exec) need
    // not lose an event, but optional metrics must never wait indefinitely.
    for attempt in 0..=20 {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Some(file)),
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if attempt == 20 {
                    return Ok(None);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}
fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn save_original(dir: &Path, stdout: &[u8], stderr: &[u8]) -> Result<String> {
    // Publish only after both streams are complete. A later locked scan removes
    // pending directories left behind by process interruption.
    for _ in 0..100 {
        let id = format!(
            "{:x}-{:x}-{:x}",
            unix_millis(),
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let destination = dir.join(&id);
        if fs::symlink_metadata(&destination).is_ok() {
            continue;
        }
        let entry = dir.join(format!("{id}.pending"));
        #[cfg(unix)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&entry) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
            Ok(()) => {}
        }
        let result = (|| -> Result<()> {
            private_open(&entry.join("stdout"), false, true)?.write_all(stdout)?;
            private_open(&entry.join("stderr"), false, true)?.write_all(stderr)?;
            fs::rename(&entry, &destination)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&entry);
            return Err(error);
        }
        return Ok(id);
    }
    bail!("could not allocate unique original ID")
}
#[derive(Debug, Serialize)]
pub struct Original {
    pub id: String,
    pub bytes: u64,
    pub unix_millis: u64,
}
fn list_originals_unlocked(dir: &Path) -> Result<Vec<Original>> {
    if let Ok(meta) = fs::symlink_metadata(dir.join("originals")) {
        ensure!(
            !meta.file_type().is_symlink(),
            "original directory must not be a symlink"
        );
    }
    let entries = match fs::read_dir(dir.join("originals")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::new();
    'entries: for entry in entries {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() && id.strip_suffix(".pending").is_some_and(valid_id) {
            fs::remove_dir_all(entry.path())?;
            continue;
        }
        if !valid_id(&id) || !entry.file_type()?.is_dir() {
            continue;
        }
        let mut bytes = 0;
        for stream in ["stdout", "stderr"] {
            let meta = match fs::symlink_metadata(entry.path().join(stream)) {
                Ok(meta) => meta,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // Older writers published the directory before both files.
                    // All writers hold this lock, so a missing stream is abandoned.
                    fs::remove_dir_all(entry.path())?;
                    continue 'entries;
                }
                Err(error) => return Err(error.into()),
            };
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "original stream must be a regular file"
            );
            bytes += meta.len();
        }
        let timestamp = entry
            .metadata()?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        result.push(Original {
            id,
            bytes,
            unix_millis: timestamp,
        });
    }
    result.sort_by(|a, b| a.unix_millis.cmp(&b.unix_millis).then(a.id.cmp(&b.id)));
    Ok(result)
}
fn evict_originals(dir: &Path, keep: &str, settings: &Settings) -> Result<()> {
    let entries = list_originals_unlocked(dir.parent().context("missing state directory")?)?;
    let mut count = entries.len();
    let mut size: u64 = entries.iter().map(|e| e.bytes).sum();
    for entry in entries {
        if entry.id != keep
            && (count > settings.originals_max_entries
                || size > settings.originals_max_bytes
                || unix_millis().saturating_sub(entry.unix_millis)
                    > settings.originals_max_days * 86_400_000)
        {
            fs::remove_dir_all(dir.join(entry.id))?;
            count -= 1;
            size -= entry.bytes;
        }
    }
    Ok(())
}
pub fn list_originals_at(dir: &Path) -> Result<Vec<Original>> {
    let _lock = lock(dir)?;
    list_originals_unlocked(dir)
}
/// Default options recover the entire stream byte-for-byte. Navigation is
/// bounded to 200 matching lines by default, at most 10,000 when requested.
#[derive(Default, Debug)]
pub struct RecallOptions {
    pub stderr: bool,
    /// One-based physical line at which searching starts.
    pub from: Option<usize>,
    /// Maximum returned lines, after literal grep filtering.
    pub lines: Option<usize>,
    pub grep: Option<String>,
}
pub fn recall_at(dir: &Path, id: &str, stderr: bool, output: &mut impl Write) -> Result<()> {
    recall_with_options_at(
        dir,
        id,
        &RecallOptions {
            stderr,
            ..Default::default()
        },
        output,
    )
}
pub fn recall_with_options_at(
    dir: &Path,
    id: &str,
    options: &RecallOptions,
    output: &mut impl Write,
) -> Result<()> {
    ensure!(valid_id(id), "invalid original ID");
    ensure!(
        options.from != Some(0),
        "--from must be a positive one-based line"
    );
    ensure!(
        options.lines.is_none_or(|n| (1..=10_000).contains(&n)),
        "--lines must be between 1 and 10000"
    );
    let _lock = lock(dir)?;
    let mut entry = dir.join("originals").join(id);
    if fs::symlink_metadata(&entry).is_err() {
        let matches: Vec<_> = list_originals_unlocked(dir)?
            .into_iter()
            .filter(|entry| entry.id.starts_with(id))
            .collect();
        ensure!(
            !matches.is_empty(),
            "original ID not found (it may have expired)"
        );
        ensure!(
            matches.len() == 1,
            "ambiguous original ID prefix; use a longer prefix"
        );
        entry = dir.join("originals").join(&matches[0].id);
    }
    ensure!(
        !fs::symlink_metadata(dir.join("originals"))?
            .file_type()
            .is_symlink()
            && !fs::symlink_metadata(&entry)?.file_type().is_symlink(),
        "original directory must not be a symlink"
    );
    let path = entry.join(if options.stderr { "stderr" } else { "stdout" });
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= ORIGINAL_LIMIT,
        "invalid original stream"
    );
    let mut file = File::open(path)?.take(ORIGINAL_LIMIT);
    // An open handle keeps this stream readable if retention unlinks it. Never
    // hold the shared state lock while waiting for the recall consumer.
    FileExt::unlock(&_lock)?;
    drop(_lock);
    if options.from.is_none() && options.lines.is_none() && options.grep.is_none() {
        io::copy(&mut file, output)?;
    } else {
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut number = 0;
        let mut returned = 0;
        while reader.read_until(b'\n', &mut line)? != 0 {
            number += 1;
            let matches = options.grep.as_ref().is_none_or(|pattern| {
                pattern.is_empty()
                    || line
                        .windows(pattern.len())
                        .any(|part| part == pattern.as_bytes())
            });
            if number >= options.from.unwrap_or(1) && matches {
                output.write_all(&line)?;
                returned += 1;
                if returned >= options.lines.unwrap_or(200) {
                    break;
                }
            }
            line.clear();
        }
    }
    Ok(())
}

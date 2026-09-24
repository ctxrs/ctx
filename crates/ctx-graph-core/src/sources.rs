//! Explicitly imported source facts, reused locally until the user imports again.
use std::{io::Write, path::Path};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    index, ingest,
    model::{FileFacts, IndexReport},
};

/// Explicitly capture a source and index it within one shared semantic-file allowance.
/// The durable source cache is published before the SQLite update, so failed updates
/// can be retried without fetching the source again.
pub fn add_and_index(
    root: &Path,
    db: &Path,
    source: &str,
    name: Option<&str>,
    options: &index::IndexOptions,
    capture: &ingest::CaptureMetadata,
) -> Result<(SourceRecord, IndexReport)> {
    let prepared = index::prepare_semantic_budget(options);
    add_and_index_prepared(root, db, source, name, &prepared, capture)
        .map_err(|error| index::retain_semantic_usage(error, &prepared))
}

fn add_and_index_prepared(
    root: &Path,
    db: &Path,
    source: &str,
    name: Option<&str>,
    options: &index::IndexOptions,
    capture: &ingest::CaptureMetadata,
) -> Result<(SourceRecord, IndexReport)> {
    let capture_started = std::time::Instant::now();
    let reserved = usize::from(options.ingest.semantic.is_some());
    ensure!(
        options.max_semantic_files <= 100_000,
        "max_semantic_files must be at most 100000"
    );
    ensure!(
        reserved <= options.max_semantic_files,
        "semantic extraction exceeds configured file budget"
    );
    let previous = previous_capture(root, source)?;
    let record = extract_with_capture(source, name, &options.ingest, capture)?;
    let store = db
        .try_exists()?
        .then(|| crate::store::Store::open_read_only(db))
        .transpose()?;
    let mut losses = match &store {
        Some(store) => store.semantic_losses(std::slice::from_ref(&record.facts))?,
        None => Vec::new(),
    };
    let cached_loss = previous.as_ref().is_some_and(|(old, _)| {
        let (old_nodes, old_edges) = index::semantic_counts(&old.facts);
        let (nodes, edges) = index::semantic_counts(&record.facts);
        nodes < old_nodes || edges < old_edges
    });
    if cached_loss {
        losses.push("saved source capture".into());
    }
    if !losses.is_empty() {
        ensure!(
            options.allow_semantic_shrink,
            "semantic capture would remove recorded facts from {}; previous source cache and graph retained. Use --allow-semantic-shrink to accept it with a backup",
            losses.join(", ")
        );
        if let Some(store) = &store {
            index::preserve_snapshot(root, store)?;
        }
        if let Some((_, bytes)) = &previous {
            index::preserve_backup(root, "source", bytes)?;
        }
    }
    let record = save(root, source, record.facts)?;
    drop(store);
    let capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
    let mut report = index::run_with_reserved_semantic_files(root, db, options, reserved)?;
    if let Some(timings) = &mut report.timings {
        timings.capture_ms = Some(capture_ms);
        timings.total_ms += capture_ms;
    }
    Ok((record, report))
}

const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub schema_version: u32,
    pub source: String,
    pub facts: FileFacts,
}

fn previous_capture(root: &Path, source: &str) -> Result<Option<(SourceRecord, Vec<u8>)>> {
    let directory = root.join(".graf/sources");
    for directory in [root.join(".graf"), directory.clone()] {
        match std::fs::symlink_metadata(directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("cannot inspect source cache"),
            Ok(m) => ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "source cache must be a regular directory"
            ),
        }
    }
    let key = blake3::hash(source.as_bytes()).to_hex();
    let path = directory.join(format!("{key}.json"));
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("cannot inspect source capture"),
        Ok(_) => (),
    }
    let bytes = index::read_source(&path, MAX_RECORD_BYTES)?
        .1
        .context("source cache exceeds 64 MiB")?;
    let record: SourceRecord = serde_json::from_slice(&bytes).context("invalid source cache")?;
    let name = record
        .facts
        .path
        .rsplit('/')
        .next()
        .context("missing source filename")?;
    ensure!(
        record.schema_version == 1
            && record.source == source
            && record.facts.path == relative_path(source, name)?,
        "source cache identity mismatch"
    );
    Ok(Some((record, bytes)))
}

pub fn relative_path(source: &str, name: &str) -> Result<String> {
    ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0']),
        "name must be a single filename"
    );
    let key = blake3::hash(source.as_bytes()).to_hex();
    Ok(format!(".graf/sources/{key}/{name}"))
}

/// Importing performs the requested download/conversion/model work once. Updating
/// the index later reads these facts; it never re-fetches their source.
pub fn add(
    root: &Path,
    source: &str,
    name: Option<&str>,
    options: &ingest::IngestOptions,
) -> Result<SourceRecord> {
    add_with_capture(
        root,
        source,
        name,
        options,
        &ingest::CaptureMetadata::default(),
    )
}

pub fn add_with_capture(
    root: &Path,
    source: &str,
    name: Option<&str>,
    options: &ingest::IngestOptions,
    capture: &ingest::CaptureMetadata,
) -> Result<SourceRecord> {
    let record = extract_with_capture(source, name, options, capture)?;
    save(root, source, record.facts)
}

fn extract_with_capture(
    source: &str,
    name: Option<&str>,
    options: &ingest::IngestOptions,
    capture: &ingest::CaptureMetadata,
) -> Result<SourceRecord> {
    let remote = source.starts_with("https://") || source.starts_with("http://");
    let inferred = if remote {
        let url = reqwest::Url::parse(source)?;
        url.path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("page.html")
            .to_owned()
    } else {
        Path::new(source)
            .file_name()
            .and_then(|s| s.to_str())
            .context("source requires a UTF-8 filename")?
            .to_owned()
    };
    let mut name = name.unwrap_or(&inferred).to_owned();
    if remote && Path::new(&name).extension().is_none() {
        name.push_str(".html");
    }
    let relative = relative_path(source, &name)?;
    let mut facts = if remote {
        ingest::extract_url(source, &relative, options)?
    } else {
        let path = Path::new(source);
        match ingest::extension(path).as_str() {
            "gdoc" | "gsheet" | "gslides" => ingest::extract_google(path, &relative, options)?,
            _ => {
                let (hash, bytes) = index::read_source(path, options.max_input_bytes)?;
                let bytes = bytes.context("source exceeds input byte limit")?;
                let extension = Path::new(&name)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let mut staged = tempfile::Builder::new()
                    .suffix(&format!(".{extension}"))
                    .tempfile()?;
                staged.write_all(&bytes)?;
                staged.flush()?;
                let hash = format!("{}:{hash}", ingest::config_fingerprint(options)?);
                ingest::extract(staged.path(), &relative, &hash, options)?
            }
        }
    };
    ingest::apply_capture_metadata(&mut facts, capture)?;
    Ok(SourceRecord {
        schema_version: 1,
        source: source.to_owned(),
        facts,
    })
}

/// Save already extracted facts from an explicit adapter, without a network call.
pub fn save(root: &Path, source: &str, facts: FileFacts) -> Result<SourceRecord> {
    let key = blake3::hash(source.as_bytes()).to_hex().to_string();
    let name = facts
        .path
        .rsplit('/')
        .next()
        .context("source facts have no filename")?;
    ensure!(
        facts.path == relative_path(source, name)?,
        "source facts path differs from source identity"
    );
    ensure!(
        facts.nodes.iter().all(|n| n.file == facts.path),
        "source node file differs from source identity"
    );
    let record = SourceRecord {
        schema_version: 1,
        source: source.to_owned(),
        facts,
    };
    let bytes = serde_json::to_vec(&record)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECORD_BYTES,
        "extracted source record exceeds 64 MiB"
    );
    let directory = root.join(".graf/sources");
    for directory in [root.join(".graf"), directory.clone()] {
        match std::fs::create_dir(&directory) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&directory)?;
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "source cache must be a regular directory"
                );
            }
            Err(e) => return Err(e).context("cannot create source cache"),
        }
    }
    let destination = directory.join(format!("{key}.json"));
    if let Ok(metadata) = std::fs::symlink_metadata(&destination) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "source cache entry must be a regular file"
        );
    }
    let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist(&destination)
        .map_err(|e| e.error)
        .context("cannot save source cache")?;
    Ok(record)
}

pub(crate) fn read(root: &Path) -> Result<Vec<FileFacts>> {
    let directory = root.join(".graf/sources");
    match std::fs::symlink_metadata(&directory) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).context("cannot inspect source cache"),
        Ok(m) => ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "source cache must be a regular directory"
        ),
    }
    let mut records = vec![];
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(key) = name.strip_suffix(".json") else {
            continue;
        };
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid source cache filename"
        );
        let (hash, bytes) = index::read_source(&entry.path(), MAX_RECORD_BYTES)?;
        let mut record: SourceRecord =
            serde_json::from_slice(&bytes.context("source cache exceeds 64 MiB")?)
                .context("invalid source cache")?;
        ensure!(
            record.schema_version == 1,
            "unsupported source cache schema"
        );
        ensure!(
            blake3::hash(record.source.as_bytes()).to_hex().as_str() == key,
            "source cache identity mismatch"
        );
        let prefix = format!(".graf/sources/{key}/");
        let name = record
            .facts
            .path
            .strip_prefix(&prefix)
            .context("source cache path mismatch")?;
        ensure!(
            !name.is_empty()
                && !name.contains(['/', '\\', ':', '\0'])
                && name != "."
                && name != "..",
            "source cache filename is invalid"
        );
        record.facts.hash = format!("managed-v1:{hash}");
        records.push(record.facts);
    }
    records.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(records)
}

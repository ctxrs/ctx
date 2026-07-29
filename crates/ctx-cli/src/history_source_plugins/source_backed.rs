use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};
use ctx_history_core::{
    platform_security::{restrict_private_directory, restrict_private_file},
    CaptureProvider, CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord,
    CtxHistoryJsonlFileTouchRecord, CtxHistoryJsonlManifestRecord, CtxHistoryJsonlRecord,
    CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    runtime::{run_history_source_plugin, HistorySourcePluginRunOptions},
    HistorySourcePluginSource,
};
use crate::identity;

const ROUTE_DIRECTORY: &str = "history-source-plugin-sources";
const SOURCE_FILE: &str = "source.jsonl";
const CURSOR_FILE: &str = "cursor.json";
const LOCK_FILE: &str = "route.lock";
const CURSOR_SCHEMA_VERSION: u16 = 1;
const MAX_CURSOR_STATE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 1024 * 1024;
const MAX_PLUGIN_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MANAGED_SOURCE_BYTES: usize = 1024 * 1024 * 1024;
const ROUTE_SOURCE_FORMAT: &str = "ctx_history_jsonl_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistorySourcePluginWorkKind {
    Cold,
    NoOp,
    Append,
    Rewrite,
    Reset,
}

impl HistorySourcePluginWorkKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::NoOp => "no_op",
            Self::Append => "append",
            Self::Rewrite => "rewrite",
            Self::Reset => "reset",
        }
    }

    pub(crate) fn changed(self) -> bool {
        !matches!(self, Self::NoOp)
    }
}

#[derive(Debug)]
pub(crate) struct PreparedHistorySourcePluginRefresh {
    source: HistorySourcePluginSource,
    provider_source: ProviderSource,
    cursor_path: PathBuf,
    cursor_after: Option<String>,
    cursor_stream: String,
    machine_id: String,
    lock: File,
    pub(crate) work_kind: HistorySourcePluginWorkKind,
    pub(crate) imported_sessions: usize,
    pub(crate) imported_events: usize,
    pub(crate) imported_edges: usize,
    pub(crate) skipped_records: usize,
    pub(crate) plugin_stderr: String,
}

impl PreparedHistorySourcePluginRefresh {
    pub(crate) fn source(&self) -> &HistorySourcePluginSource {
        &self.source
    }

    pub(crate) fn provider_source(&self) -> &ProviderSource {
        &self.provider_source
    }

    pub(crate) fn snapshot_path(&self) -> &Path {
        &self.provider_source.path
    }

    /// Cursor state advances only after the daemon has published the managed
    /// provider-export source. A failed publication can safely retry because
    /// stream merging is idempotent.
    pub(crate) fn commit_cursor(&self) -> Result<()> {
        let Some(cursor) = self.cursor_after.as_deref() else {
            return Ok(());
        };
        let state = PluginCursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            plugin_name: self.source.plugin_name.clone(),
            provider_key: self.source.provider_key.clone(),
            source_id: self.source.source_id.clone(),
            source_format: self.source.source_format.clone(),
            cursor_stream: self.cursor_stream.clone(),
            machine_id: self.machine_id.clone(),
            cursor: cursor.to_owned(),
        };
        let mut bytes = serde_json::to_vec_pretty(&state)?;
        bytes.push(b'\n');
        if fs::read(&self.cursor_path).ok().as_deref() == Some(bytes.as_slice()) {
            return Ok(());
        }
        write_private_atomic(&self.cursor_path, &bytes)
            .context("commit history source plugin cursor")
    }
}

impl Drop for PreparedHistorySourcePluginRefresh {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PluginCursorState {
    schema_version: u16,
    plugin_name: String,
    provider_key: String,
    source_id: String,
    source_format: String,
    cursor_stream: String,
    machine_id: String,
    cursor: String,
}

#[derive(Debug)]
struct ParsedPluginStream {
    manifest: CtxHistoryJsonlManifestRecord,
    source: CtxHistoryJsonlSourceRecord,
    sessions: BTreeMap<(String, String), CtxHistoryJsonlSessionRecord>,
    events: BTreeMap<(String, String, u64), CtxHistoryJsonlEventRecord>,
    touches: BTreeMap<(String, String, u64), CtxHistoryJsonlFileTouchRecord>,
    edges: BTreeMap<(String, String, String, String), CtxHistoryJsonlEdgeRecord>,
}

#[derive(Debug, Default)]
struct MergeFacts {
    inserted_sessions: usize,
    inserted_events: usize,
    inserted_edges: usize,
    skipped_records: usize,
    replaced_records: usize,
}

impl MergeFacts {
    fn work_kind(
        &self,
        had_snapshot: bool,
        reset: bool,
        bytes_changed: bool,
    ) -> HistorySourcePluginWorkKind {
        if !had_snapshot {
            return HistorySourcePluginWorkKind::Cold;
        }
        if !bytes_changed {
            return HistorySourcePluginWorkKind::NoOp;
        }
        if reset {
            return HistorySourcePluginWorkKind::Reset;
        }
        if self.replaced_records == 0 {
            HistorySourcePluginWorkKind::Append
        } else {
            HistorySourcePluginWorkKind::Rewrite
        }
    }
}

pub(crate) fn prepare_source_backed_history_source(
    source: HistorySourcePluginSource,
    data_root: &Path,
    full_rescan: bool,
) -> Result<PreparedHistorySourcePluginRefresh> {
    let route_root = managed_route_root(data_root, &source);
    fs::create_dir_all(&route_root).with_context(|| {
        format!(
            "create history source plugin route {}",
            route_root.display()
        )
    })?;
    restrict_private_directory(&route_root).with_context(|| {
        format!(
            "protect history source plugin route {}",
            route_root.display()
        )
    })?;
    let lock = open_route_lock(&route_root)?;
    lock.lock_exclusive()
        .context("lock history source plugin route")?;

    let cursor_path = route_root.join(CURSOR_FILE);
    let machine_id = identity::installation_id(data_root)?;
    let cursor_stream = source.cursor_stream();
    let previous_cursor = if full_rescan {
        None
    } else {
        read_cursor_state(&cursor_path, &source, &cursor_stream, &machine_id)?
            .map(|state| state.cursor)
    };
    let run = run_history_source_plugin(
        &source,
        HistorySourcePluginRunOptions {
            data_root,
            machine_id: &machine_id,
            cursor: previous_cursor.as_deref(),
            cursor_stream: &cursor_stream,
            full_rescan,
        },
    )?;
    let mut delta = parse_plugin_stream(&source, &run.stdout)
        .with_context(|| format!("validate history source plugin {} output", source.label()))?;
    let cursor_after = validate_plugin_output_identity(
        &source,
        &delta,
        &cursor_stream,
        &machine_id,
        previous_cursor.as_deref(),
        full_rescan,
    )?;
    delta.source.cursor = None;
    annotate_plugin_source(&source, &mut delta.source);

    let snapshot_path = route_root.join(SOURCE_FILE);
    let previous_bytes = read_optional_bounded(&snapshot_path, MAX_MANAGED_SOURCE_BYTES)?;
    let had_snapshot = previous_bytes.is_some();
    let mut facts = MergeFacts::default();
    let merged = if !full_rescan {
        match previous_bytes.as_deref() {
            Some(bytes) => {
                let mut current = parse_plugin_stream(&source, bytes).with_context(|| {
                    format!(
                        "validate managed history source plugin snapshot {}",
                        snapshot_path.display()
                    )
                })?;
                merge_plugin_stream(&mut current, delta, &mut facts);
                current
            }
            None => {
                facts.inserted_sessions = delta.sessions.len();
                facts.inserted_events = delta.events.len();
                facts.inserted_edges = delta.edges.len();
                delta
            }
        }
    } else {
        facts.inserted_sessions = delta.sessions.len();
        facts.inserted_events = delta.events.len();
        facts.inserted_edges = delta.edges.len();
        delta
    };
    validate_merged_references(&source, &merged)?;
    let merged_bytes = serialize_plugin_stream(&merged)?;
    let bytes_changed = previous_bytes.as_deref() != Some(merged_bytes.as_slice());
    if bytes_changed {
        write_private_atomic(&snapshot_path, &merged_bytes).with_context(|| {
            format!(
                "publish managed history source plugin snapshot {}",
                snapshot_path.display()
            )
        })?;
    }
    let work_kind = facts.work_kind(had_snapshot, full_rescan, bytes_changed);
    let provider_source = managed_custom_provider_source(snapshot_path);

    Ok(PreparedHistorySourcePluginRefresh {
        source,
        provider_source,
        cursor_path,
        cursor_after,
        cursor_stream,
        machine_id,
        lock,
        work_kind,
        imported_sessions: facts.inserted_sessions,
        imported_events: facts.inserted_events,
        imported_edges: facts.inserted_edges,
        skipped_records: facts.skipped_records,
        plugin_stderr: run.stderr,
    })
}

fn managed_route_root(data_root: &Path, source: &HistorySourcePluginSource) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(b"ctx.history-source-plugin-route.v1\0");
    digest.update(source.provider_key.as_bytes());
    digest.update(b"\0");
    digest.update(source.source_id.as_bytes());
    digest.update(b"\0");
    digest.update(source.source_format.as_bytes());
    let digest = encode_hex(&digest.finalize());
    data_root
        .join(ROUTE_DIRECTORY)
        .join(&source.provider_key)
        .join(format!("{}-{}", source.source_id, &digest[..16]))
}

fn open_route_lock(route_root: &Path) -> Result<File> {
    let path = route_root.join(LOCK_FILE);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("open history source plugin lock {}", path.display()))?;
    restrict_private_file(&path)
        .with_context(|| format!("protect history source plugin lock {}", path.display()))?;
    Ok(file)
}

fn read_cursor_state(
    path: &Path,
    source: &HistorySourcePluginSource,
    cursor_stream: &str,
    machine_id: &str,
) -> Result<Option<PluginCursorState>> {
    let Some(bytes) = read_optional_bounded(path, MAX_CURSOR_STATE_BYTES as usize)? else {
        return Ok(None);
    };
    let state: PluginCursorState = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse history source plugin cursor {}", path.display()))?;
    if state.schema_version != CURSOR_SCHEMA_VERSION
        || state.plugin_name != source.plugin_name
        || state.provider_key != source.provider_key
        || state.source_id != source.source_id
        || state.source_format != source.source_format
        || state.cursor_stream != cursor_stream
        || state.machine_id != machine_id
        || state.cursor.len() > MAX_CURSOR_BYTES
    {
        bail!(
            "history source plugin cursor state does not match selected source {}",
            source.label()
        );
    }
    Ok(Some(state))
}

fn parse_plugin_stream(
    source: &HistorySourcePluginSource,
    bytes: &[u8],
) -> Result<ParsedPluginStream> {
    let mut manifest = None;
    let mut source_record = None;
    let mut sessions = BTreeMap::new();
    let mut events = BTreeMap::new();
    let mut touches = BTreeMap::new();
    let mut edges = BTreeMap::new();
    for (line_number, line) in jsonl_lines(source, bytes)? {
        if line.trim().is_empty() {
            continue;
        }
        let record: CtxHistoryJsonlRecord = serde_json::from_str(line).with_context(|| {
            format!(
                "history source plugin {} emitted invalid ctx-history-jsonl-v1 at line {line_number}",
                source.label()
            )
        })?;
        match record {
            CtxHistoryJsonlRecord::Manifest(record) => {
                if manifest.replace(record).is_some() {
                    bail!(
                        "history source plugin {} emitted duplicate manifest record at line {line_number}",
                        source.label()
                    );
                }
            }
            CtxHistoryJsonlRecord::Source(record) => {
                if source_record.replace(record).is_some() {
                    bail!(
                        "history source plugin {} emitted duplicate source record at line {line_number}",
                        source.label()
                    );
                }
            }
            CtxHistoryJsonlRecord::Session(record) => {
                let key = (record.source_id.clone(), record.session_id.clone());
                if sessions.insert(key, record).is_some() {
                    bail!(
                        "history source plugin {} emitted duplicate session record at line {line_number}",
                        source.label()
                    );
                }
            }
            CtxHistoryJsonlRecord::Event(record) => {
                let key = (
                    record.source_id.clone(),
                    record.session_id.clone(),
                    record.event_index,
                );
                if events.insert(key, record).is_some() {
                    bail!(
                        "history source plugin {} emitted duplicate event record at line {line_number}",
                        source.label()
                    );
                }
            }
            CtxHistoryJsonlRecord::FileTouch(record) => {
                let key = (
                    record.source_id.clone(),
                    record.session_id.clone(),
                    record.touch_index,
                );
                if touches.insert(key, record).is_some() {
                    bail!(
                        "history source plugin {} emitted duplicate file_touch record at line {line_number}",
                        source.label()
                    );
                }
            }
            CtxHistoryJsonlRecord::Edge(record) => {
                let identity = record.edge_id.clone().unwrap_or_else(|| {
                    format!(
                        "{}:{}:{}",
                        record.from_session_id,
                        record.to_session_id,
                        record.edge_type.as_str()
                    )
                });
                let key = (
                    record.source_id.clone(),
                    record.from_session_id.clone(),
                    record.to_session_id.clone(),
                    identity,
                );
                if edges.insert(key, record).is_some() {
                    bail!(
                        "history source plugin {} emitted duplicate edge record at line {line_number}",
                        source.label()
                    );
                }
            }
        }
    }
    let manifest = manifest.ok_or_else(|| {
        anyhow!(
            "history source plugin {} emitted no manifest record",
            source.label()
        )
    })?;
    if manifest.schema_version != CTX_HISTORY_JSONL_V1_SCHEMA_VERSION {
        bail!(
            "history source plugin {} emitted unsupported schema_version `{}`",
            source.label(),
            manifest.schema_version
        );
    }
    let source_record = source_record.ok_or_else(|| {
        anyhow!(
            "history source plugin {} emitted no source record",
            source.label()
        )
    })?;
    Ok(ParsedPluginStream {
        manifest,
        source: source_record,
        sessions,
        events,
        touches,
        edges,
    })
}

fn validate_plugin_output_identity(
    source: &HistorySourcePluginSource,
    stream: &ParsedPluginStream,
    cursor_stream: &str,
    machine_id: &str,
    previous_cursor: Option<&str>,
    require_after_cursor: bool,
) -> Result<Option<String>> {
    let record = &stream.source;
    if record.provider_key != source.provider_key
        || record.source_id != source.source_id
        || record.source_format != source.source_format
    {
        bail!(
            "history source plugin {} emitted source identity {}/{}/{} but manifest declares {}/{}/{}",
            source.label(),
            record.provider_key,
            record.source_id,
            record.source_format,
            source.provider_key,
            source.source_id,
            source.source_format
        );
    }
    if let Some(emitted_machine_id) = record.machine_id.as_deref() {
        if emitted_machine_id != machine_id {
            bail!(
                "history source plugin {} emitted machine_id `{emitted_machine_id}` but ctx is importing as `{machine_id}`; omit machine_id or set it to CTX_HISTORY_MACHINE_ID",
                source.label()
            );
        }
    }
    let before = record
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.before.as_ref());
    if let Some(before) = before {
        if before.stream != cursor_stream || previous_cursor != Some(before.cursor.as_str()) {
            bail!(
                "history source plugin {} emitted source.cursor.before that does not match the supplied cursor",
                source.label()
            );
        }
    }
    let after = record
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.after.as_ref());
    if require_after_cursor && after.is_none() {
        bail!(
            "history source plugin {} was reset but emitted no source.cursor.after checkpoint; emit a fresh cursor after a full rescan",
            source.label()
        );
    }
    let Some(after) = after else {
        return Ok(None);
    };
    if after.stream != cursor_stream {
        bail!(
            "history source plugin {} emitted source.cursor.after stream `{}` but expected `{cursor_stream}`",
            source.label(),
            after.stream
        );
    }
    if after.cursor.len() > MAX_CURSOR_BYTES {
        bail!(
            "history source plugin {} emitted a cursor exceeding {MAX_CURSOR_BYTES} bytes",
            source.label()
        );
    }
    Ok(Some(after.cursor.clone()))
}

fn annotate_plugin_source(
    source: &HistorySourcePluginSource,
    record: &mut CtxHistoryJsonlSourceRecord,
) {
    let mut metadata = match std::mem::take(&mut record.metadata) {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("provider_metadata".to_owned(), other);
            map
        }
    };
    metadata.insert(
        "ctx_history_plugin".to_owned(),
        json!({
            "plugin_name": source.plugin_name,
            "plugin_source_id": source.id,
            "history_source": source.history_source(),
            "plugin_source": source.label(),
            "plugin_display_name": source.plugin_display_name,
            "plugin_version": source.plugin_version,
            "manifest_path": source.manifest_path,
            "provider_key": source.provider_key,
            "source_id": source.source_id,
            "source_format": source.source_format,
        }),
    );
    record.metadata = Value::Object(metadata);
}

fn merge_plugin_stream(
    current: &mut ParsedPluginStream,
    delta: ParsedPluginStream,
    facts: &mut MergeFacts,
) {
    let (inserted, skipped, replaced) = merge_records(&mut current.sessions, delta.sessions);
    facts.inserted_sessions = facts.inserted_sessions.saturating_add(inserted);
    facts.skipped_records = facts.skipped_records.saturating_add(skipped);
    facts.replaced_records = facts.replaced_records.saturating_add(replaced);

    let (inserted, skipped, replaced) = merge_records(&mut current.events, delta.events);
    facts.inserted_events = facts.inserted_events.saturating_add(inserted);
    facts.skipped_records = facts.skipped_records.saturating_add(skipped);
    facts.replaced_records = facts.replaced_records.saturating_add(replaced);

    let (_, skipped, replaced) = merge_records(&mut current.touches, delta.touches);
    facts.skipped_records = facts.skipped_records.saturating_add(skipped);
    facts.replaced_records = facts.replaced_records.saturating_add(replaced);

    let (inserted, skipped, replaced) = merge_records(&mut current.edges, delta.edges);
    facts.inserted_edges = facts.inserted_edges.saturating_add(inserted);
    facts.skipped_records = facts.skipped_records.saturating_add(skipped);
    facts.replaced_records = facts.replaced_records.saturating_add(replaced);
}

fn merge_records<K, V>(current: &mut BTreeMap<K, V>, delta: BTreeMap<K, V>) -> (usize, usize, usize)
where
    K: Ord,
    V: PartialEq,
{
    let mut inserted = 0_usize;
    let mut skipped = 0_usize;
    let mut replaced = 0_usize;
    for (key, value) in delta {
        match current.get(&key) {
            Some(existing) if existing == &value => {
                skipped = skipped.saturating_add(1);
            }
            Some(_) => {
                current.insert(key, value);
                replaced = replaced.saturating_add(1);
            }
            None => {
                current.insert(key, value);
                inserted = inserted.saturating_add(1);
            }
        }
    }
    (inserted, skipped, replaced)
}

fn validate_merged_references(
    source: &HistorySourcePluginSource,
    stream: &ParsedPluginStream,
) -> Result<()> {
    for ((source_id, session_id), session) in &stream.sessions {
        if source_id != &source.source_id
            || session.source_id != source.source_id
            || session.session_id != *session_id
        {
            bail!(
                "history source plugin {} emitted a session outside source_id `{}`",
                source.label(),
                source.source_id
            );
        }
        for dependency in [
            session.parent_session_id.as_deref(),
            session.root_session_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if dependency != session_id
                && !stream
                    .sessions
                    .contains_key(&(source.source_id.clone(), dependency.to_owned()))
            {
                bail!(
                    "history source plugin {} session `{session_id}` references unknown session `{dependency}`",
                    source.label()
                );
            }
        }
        validate_session_parent_chain(source, stream, session_id)?;
    }
    for ((source_id, session_id, event_index), event) in &stream.events {
        if source_id != &source.source_id
            || event.source_id != source.source_id
            || event.session_id != *session_id
            || event.event_index != *event_index
            || !stream
                .sessions
                .contains_key(&(source_id.clone(), session_id.clone()))
        {
            bail!(
                "history source plugin {} event {session_id}/{event_index} has invalid source or session identity",
                source.label()
            );
        }
    }
    for ((source_id, session_id, touch_index), touch) in &stream.touches {
        if source_id != &source.source_id
            || touch.source_id != source.source_id
            || touch.session_id != *session_id
            || touch.touch_index != *touch_index
            || touch.path.trim().is_empty()
            || !stream
                .sessions
                .contains_key(&(source_id.clone(), session_id.clone()))
        {
            bail!(
                "history source plugin {} file_touch {session_id}/{touch_index} has invalid source, session, or path",
                source.label()
            );
        }
        if let Some(event_index) = touch.event_index {
            if !stream
                .events
                .contains_key(&(source_id.clone(), session_id.clone(), event_index))
            {
                bail!(
                    "history source plugin {} file_touch references unknown event {session_id}/{event_index}",
                    source.label()
                );
            }
        }
    }
    for edge in stream.edges.values() {
        if edge.source_id != source.source_id
            || !stream
                .sessions
                .contains_key(&(source.source_id.clone(), edge.from_session_id.clone()))
            || !stream
                .sessions
                .contains_key(&(source.source_id.clone(), edge.to_session_id.clone()))
        {
            bail!(
                "history source plugin {} edge references an unknown source or session",
                source.label()
            );
        }
    }
    Ok(())
}

fn validate_session_parent_chain(
    source: &HistorySourcePluginSource,
    stream: &ParsedPluginStream,
    session_id: &str,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut current = session_id;
    loop {
        if !seen.insert(current.to_owned()) {
            bail!(
                "history source plugin {} session `{session_id}` has a cyclic parent relationship",
                source.label()
            );
        }
        let session = &stream.sessions[&(source.source_id.clone(), current.to_owned())];
        let Some(parent) = session.parent_session_id.as_deref() else {
            return Ok(());
        };
        if parent == current {
            bail!(
                "history source plugin {} session `{session_id}` is its own parent",
                source.label()
            );
        }
        current = parent;
    }
}

fn serialize_plugin_stream(stream: &ParsedPluginStream) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write_record(
        &mut bytes,
        CtxHistoryJsonlRecord::Manifest(stream.manifest.clone()),
    )?;
    write_record(
        &mut bytes,
        CtxHistoryJsonlRecord::Source(stream.source.clone()),
    )?;
    for record in stream.sessions.values() {
        write_record(&mut bytes, CtxHistoryJsonlRecord::Session(record.clone()))?;
    }
    for record in stream.events.values() {
        write_record(&mut bytes, CtxHistoryJsonlRecord::Event(record.clone()))?;
    }
    for record in stream.touches.values() {
        write_record(&mut bytes, CtxHistoryJsonlRecord::FileTouch(record.clone()))?;
    }
    for record in stream.edges.values() {
        write_record(&mut bytes, CtxHistoryJsonlRecord::Edge(record.clone()))?;
    }
    if bytes.len() > MAX_MANAGED_SOURCE_BYTES {
        bail!("managed history source plugin snapshot exceeds {MAX_MANAGED_SOURCE_BYTES} bytes");
    }
    Ok(bytes)
}

fn write_record(bytes: &mut Vec<u8>, record: CtxHistoryJsonlRecord) -> Result<()> {
    serde_json::to_writer(&mut *bytes, &record)?;
    bytes.push(b'\n');
    Ok(())
}

fn jsonl_lines<'a>(
    source: &HistorySourcePluginSource,
    bytes: &'a [u8],
) -> Result<Vec<(usize, &'a str)>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut line_number = 1_usize;
    for (index, byte) in bytes.iter().enumerate() {
        let length = index.saturating_add(1).saturating_sub(start);
        if length > MAX_PLUGIN_JSONL_LINE_BYTES {
            bail!(
                "history source plugin {} emitted ctx-history-jsonl-v1 line {line_number} exceeding max bytes ({MAX_PLUGIN_JSONL_LINE_BYTES})",
                source.label()
            );
        }
        if *byte == b'\n' {
            let line = std::str::from_utf8(&bytes[start..index]).with_context(|| {
                format!(
                    "history source plugin {} emitted non-UTF-8 output at line {line_number}",
                    source.label()
                )
            })?;
            lines.push((line_number, line.strip_suffix('\r').unwrap_or(line)));
            start = index.saturating_add(1);
            line_number = line_number.saturating_add(1);
        }
    }
    if start < bytes.len() {
        let length = bytes.len().saturating_sub(start);
        if length > MAX_PLUGIN_JSONL_LINE_BYTES {
            bail!(
                "history source plugin {} emitted ctx-history-jsonl-v1 line {line_number} exceeding max bytes ({MAX_PLUGIN_JSONL_LINE_BYTES})",
                source.label()
            );
        }
        let line = std::str::from_utf8(&bytes[start..]).with_context(|| {
            format!(
                "history source plugin {} emitted non-UTF-8 output at line {line_number}",
                source.label()
            )
        })?;
        lines.push((line_number, line.strip_suffix('\r').unwrap_or(line)));
    }
    Ok(lines)
}

fn managed_custom_provider_source(path: PathBuf) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Custom,
        path,
        exists: true,
        source_format: ROUTE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn read_optional_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("open bounded file {}", path.display()));
        }
    };
    let mut bytes = Vec::new();
    file.take((maximum as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read bounded file {}", path.display()))?;
    if bytes.len() > maximum {
        bail!("file exceeds {maximum} byte bound: {}", path.display());
    }
    Ok(Some(bytes))
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("private state path has no parent: {}", path.display()))?;
    let staged = parent.join(format!(".history-source-plugin-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&staged)
        .with_context(|| format!("create staged plugin state {}", staged.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)
            .with_context(|| format!("write staged plugin state {}", staged.display()))?;
        file.sync_all()
            .with_context(|| format!("sync staged plugin state {}", staged.display()))?;
        replace_file(&staged, path)?;
        restrict_private_file(path)
            .with_context(|| format!("protect plugin state {}", path.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(staged: &Path, destination: &Path) -> Result<()> {
    fs::rename(staged, destination).with_context(|| {
        format!(
            "replace history source plugin state {} with {}",
            destination.display(),
            staged.display()
        )
    })
}

#[cfg(windows)]
fn replace_file(staged: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let mut staged_wide = staged.as_os_str().encode_wide().collect::<Vec<_>>();
    staged_wide.push(0);
    let mut destination_wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    destination_wide.push(0);
    let result = unsafe {
        MoveFileExW(
            staged_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "replace history source plugin state {}",
                destination.display()
            )
        });
    }
    Ok(())
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync history source plugin directory {}", path.display()))
}

#[cfg(target_os = "macos")]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn sync_directory(path: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .write(true)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .sync_all()
        .with_context(|| format!("sync history source plugin directory {}", path.display()))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use ctx_history_source_io::{
    provider_metadata_is_link_like, provider_safe_path_segment, MAX_PROVIDER_JSONL_LINE_BYTES,
};
use serde::{
    de::{Error as _, IgnoredAny, MapAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;

use super::{
    open_ordinary_file_without_following, path_metadata_probe, sorted_probe_entries, BoundedProbe,
    PathProbe, MAX_DIRECT_DIRECTORY_ENTRIES,
};

const FX_AUTHORITY_MAX_BYTES: u64 = 16 * 1024;
// fx permits one encoded event frame, including its newline, up to 8 MiB.
const FX_FIRST_EVENT_MAX_BYTES: u64 = 8 * 1024 * 1024;
const FX_WATERMARK_MAX_BYTES: u64 = 16 * 1024;
pub(super) const FX_LEGACY_SUMMARY_PREFIX_MAX_BYTES: u64 = 64 * 1024;
const FX_LEGACY_KEY_MAX_BYTES: usize = 64;
const FX_LEGACY_ID_MAX_BYTES: usize = 255;
const FX_LEGACY_LANGUAGE_MAX_BYTES: usize = 24;
const FX_LEGACY_WORKSPACE_MAX_BYTES: usize = 4 * 1024;
const FX_LEGACY_JSON_MAX_DEPTH: usize = 64;

pub(super) fn has_fx_session_under_immediate_child(
    root: &Path,
    max_entries: usize,
) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::File | PathProbe::Other => {
            return BoundedProbe::NotFound;
        }
        PathProbe::IoError => return BoundedProbe::IoError,
    }

    let root_entries = match sorted_probe_entries(root, max_entries) {
        Ok(entries) => entries,
        Err(outcome) => return outcome,
    };
    let mut visited = root_entries.len();
    let mut session_directories = Vec::new();
    for path in root_entries {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => return BoundedProbe::IoError,
        };
        if !provider_metadata_is_link_like(&metadata) && metadata.file_type().is_dir() {
            session_directories.push(path);
        }
    }

    for session_dir in session_directories {
        let entries = match sorted_probe_entries(&session_dir, max_entries.saturating_sub(visited))
        {
            Ok(entries) => entries,
            Err(BoundedProbe::BudgetExhausted) => return BoundedProbe::BudgetExhausted,
            Err(_) => return BoundedProbe::IoError,
        };
        visited = visited.saturating_add(entries.len());
        for candidate in entries {
            let metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(_) => return BoundedProbe::IoError,
            };
            if !provider_metadata_is_link_like(&metadata)
                && metadata.file_type().is_file()
                && is_fx_session_candidate(&candidate)
            {
                return BoundedProbe::Found;
            }
        }
    }
    BoundedProbe::NotFound
}

pub(super) fn is_fx_session_candidate(candidate: &Path) -> bool {
    let Some(session_dir) = candidate.parent() else {
        return false;
    };
    if path_metadata_probe(&session_dir.join("authority.pending.json")) != PathProbe::Missing {
        return false;
    }
    match candidate.file_name().and_then(|name| name.to_str()) {
        Some("events.jsonl") => valid_fx_v3_session(candidate, session_dir),
        Some("session.json")
            if path_metadata_probe(&session_dir.join("authority.json")) == PathProbe::Missing =>
        {
            valid_fx_legacy_summary(candidate, session_dir)
        }
        _ => false,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FxV3AuthorityProbe {
    schema_version: u64,
    session_id: String,
    authority_id: String,
    storage_format: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FxV3WatermarkProbe {
    schema_version: u64,
    session_id: String,
    log_generation: String,
    through_seq: u64,
    through_event_id: String,
    through_event_log_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FxV3FirstEventProbe {
    schema_version: u64,
    log_generation: String,
    seq: u64,
    event_id: String,
    timestamp_ms: i64,
    kind: String,
    payload: Value,
}

struct FxV3FirstEventBoundary {
    log_generation: String,
    frame_bytes: u64,
}

fn valid_fx_v3_session(events_path: &Path, session_dir: &Path) -> bool {
    if path_metadata_probe(&session_dir.join("commit.pending.json")) != PathProbe::Missing {
        return false;
    }
    let Ok(events) = open_ordinary_file_without_following(events_path) else {
        return false;
    };
    let Ok(events_metadata) = events.metadata() else {
        return false;
    };
    let Some(first_event) = valid_fx_v3_first_event(events) else {
        return false;
    };
    let Some(session_id) = valid_fx_v3_authority(session_dir) else {
        return false;
    };
    let Ok(entries) = sorted_probe_entries(session_dir, MAX_DIRECT_DIRECTORY_ENTRIES) else {
        return false;
    };
    entries
        .into_iter()
        .any(|path| valid_fx_v3_watermark(&path, &session_id, &first_event, events_metadata.len()))
}

fn valid_fx_v3_first_event(events: fs::File) -> Option<FxV3FirstEventBoundary> {
    let mut reader = BufReader::new(events).take(FX_FIRST_EVENT_MAX_BYTES.saturating_add(1));
    let mut line = Vec::with_capacity(FX_FIRST_EVENT_MAX_BYTES as usize + 1);
    let read = reader.read_until(b'\n', &mut line).ok()?;
    if read == 0 || line.len() as u64 > FX_FIRST_EVENT_MAX_BYTES || line.last() != Some(&b'\n') {
        return None;
    }
    let event = serde_json::from_slice::<FxV3FirstEventProbe>(&line[..line.len() - 1]).ok()?;
    (event.schema_version == 1
        && event.seq == 1
        && event.timestamp_ms >= 0
        && event.kind == "session_started"
        && is_fx_identifier(&event.log_generation)
        && is_fx_identifier(&event.event_id)
        && matches!(event.payload, Value::Object(_)))
    .then_some(FxV3FirstEventBoundary {
        log_generation: event.log_generation,
        frame_bytes: line.len() as u64,
    })
}

fn valid_fx_v3_authority(session_dir: &Path) -> Option<String> {
    let path = session_dir.join("authority.json");
    let Ok(file) = open_ordinary_file_without_following(&path) else {
        return None;
    };
    let Ok(metadata) = file.metadata() else {
        return None;
    };
    if metadata.len() > FX_AUTHORITY_MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .take(FX_AUTHORITY_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > FX_AUTHORITY_MAX_BYTES
    {
        return None;
    }
    let Ok(authority) = serde_json::from_slice::<FxV3AuthorityProbe>(&bytes) else {
        return None;
    };
    (authority.schema_version == 1
        && authority.storage_format == "event_log_v1"
        && matches!(
            authority.source.as_str(),
            "native_create" | "legacy_migration"
        )
        && is_fx_identifier(&authority.authority_id)
        && provider_safe_path_segment(&authority.session_id)
        && session_dir.file_name().and_then(|name| name.to_str())
            == Some(authority.session_id.as_str()))
    .then_some(authority.session_id)
}

fn valid_fx_v3_watermark(
    path: &Path,
    session_id: &str,
    first_event: &FxV3FirstEventBoundary,
    events_len: u64,
) -> bool {
    let Some(generation) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("commit."))
        .and_then(|name| name.strip_suffix(".json"))
        .filter(|generation| is_fx_identifier(generation))
    else {
        return false;
    };
    let Ok(file) = open_ordinary_file_without_following(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.len() == 0 || metadata.len() > FX_WATERMARK_MAX_BYTES {
        return false;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .take(FX_WATERMARK_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > FX_WATERMARK_MAX_BYTES
    {
        return false;
    }
    let Ok(watermark) = serde_json::from_slice::<FxV3WatermarkProbe>(&bytes) else {
        return false;
    };
    watermark.schema_version == 1
        && watermark.session_id == session_id
        && watermark.log_generation == generation
        && watermark.log_generation == first_event.log_generation
        && is_fx_identifier(&watermark.through_event_id)
        && watermark.through_seq > 0
        && watermark.through_event_log_bytes >= first_event.frame_bytes
        && watermark.through_event_log_bytes <= events_len
}

fn is_fx_identifier(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Default)]
struct FxLegacySummaryProbe {
    schema_version: Option<u64>,
    id: Option<String>,
    workspace_root_seen: bool,
    created_at_ms: Option<i64>,
    updated_at_ms: Option<i64>,
    conversation_language: Option<String>,
    history_len: Option<u64>,
}

struct FxLegacyHistoryPrefix;

impl<'de> Deserialize<'de> for FxLegacyHistoryPrefix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HistoryVisitor;

        impl<'de> Visitor<'de> for HistoryVisitor {
            type Value = FxLegacyHistoryPrefix;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an fx legacy history array")
            }

            fn visit_seq<A>(self, _sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                Ok(FxLegacyHistoryPrefix)
            }
        }

        deserializer.deserialize_seq(HistoryVisitor)
    }
}

impl FxLegacySummaryProbe {
    fn complete(&self) -> bool {
        self.schema_version.is_some()
            && self.id.is_some()
            && self.created_at_ms.is_some()
            && self.updated_at_ms.is_some()
            && self.conversation_language.is_some()
            && self.history_len.is_some()
    }
}

impl<'de> Deserialize<'de> for FxLegacySummaryProbe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SummaryVisitor;

        impl<'de> Visitor<'de> for SummaryVisitor {
            type Value = FxLegacySummaryProbe;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an fx legacy session object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut summary = FxLegacySummaryProbe::default();
                while let Some(key) = map.next_key::<String>()? {
                    if key.len() > FX_LEGACY_KEY_MAX_BYTES {
                        return Err(M::Error::custom("fx legacy session key exceeds limit"));
                    }
                    if key == "history" && summary.complete() {
                        map.next_value::<FxLegacyHistoryPrefix>()?;
                        return Ok(summary);
                    }
                    match key.as_str() {
                        "schema_version" => {
                            if summary.schema_version.is_some() {
                                return Err(M::Error::custom("duplicate fx legacy schema_version"));
                            }
                            summary.schema_version = Some(map.next_value()?);
                        }
                        "id" => {
                            if summary.id.is_some() {
                                return Err(M::Error::custom("duplicate fx legacy id"));
                            }
                            let id: String = map.next_value()?;
                            if id.len() > FX_LEGACY_ID_MAX_BYTES {
                                return Err(M::Error::custom("fx legacy id exceeds limit"));
                            }
                            summary.id = Some(id);
                        }
                        "workspace_root" => {
                            if summary.workspace_root_seen {
                                return Err(M::Error::custom("duplicate fx legacy workspace_root"));
                            }
                            summary.workspace_root_seen = true;
                            let workspace: Option<String> = map.next_value()?;
                            if workspace
                                .as_deref()
                                .is_some_and(|path| path.len() > FX_LEGACY_WORKSPACE_MAX_BYTES)
                            {
                                return Err(M::Error::custom(
                                    "fx legacy workspace_root exceeds limit",
                                ));
                            }
                        }
                        "created_at_ms" => {
                            if summary.created_at_ms.is_some() {
                                return Err(M::Error::custom("duplicate fx legacy created_at_ms"));
                            }
                            summary.created_at_ms = Some(map.next_value()?);
                        }
                        "updated_at_ms" => {
                            if summary.updated_at_ms.is_some() {
                                return Err(M::Error::custom("duplicate fx legacy updated_at_ms"));
                            }
                            summary.updated_at_ms = Some(map.next_value()?);
                        }
                        "conversation_language" => {
                            if summary.conversation_language.is_some() {
                                return Err(M::Error::custom(
                                    "duplicate fx legacy conversation_language",
                                ));
                            }
                            let language: String = map.next_value()?;
                            if language.len() > FX_LEGACY_LANGUAGE_MAX_BYTES {
                                return Err(M::Error::custom(
                                    "fx legacy conversation_language exceeds limit",
                                ));
                            }
                            summary.conversation_language = Some(language);
                        }
                        "history_len" => {
                            if summary.history_len.is_some() {
                                return Err(M::Error::custom("duplicate fx legacy history_len"));
                            }
                            summary.history_len = Some(map.next_value()?);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(summary)
            }
        }

        deserializer.deserialize_map(SummaryVisitor)
    }
}

fn valid_fx_legacy_summary(path: &Path, session_dir: &Path) -> bool {
    let Ok(file) = open_ordinary_file_without_following(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.len() > MAX_PROVIDER_JSONL_LINE_BYTES as u64 {
        return false;
    }
    let mut prefix = Vec::with_capacity(FX_LEGACY_SUMMARY_PREFIX_MAX_BYTES as usize + 1);
    if file
        .take(FX_LEGACY_SUMMARY_PREFIX_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut prefix)
        .is_err()
    {
        return false;
    }
    if !fx_legacy_prefix_within_depth_limit(&prefix) {
        return false;
    }
    let mut deserializer = serde_json::Deserializer::from_slice(&prefix);
    let Ok(summary) = FxLegacySummaryProbe::deserialize(&mut deserializer) else {
        return false;
    };
    matches!(summary.schema_version, Some(1) | Some(2))
        && summary
            .id
            .as_deref()
            .is_some_and(provider_safe_path_segment)
        && summary.id.as_deref() == session_dir.file_name().and_then(|name| name.to_str())
        && summary
            .conversation_language
            .as_deref()
            .is_some_and(|language| !language.is_empty())
}

fn fx_legacy_prefix_within_depth_limit(bytes: &[u8]) -> bool {
    let mut stack = [0_u8; FX_LEGACY_JSON_MAX_DEPTH];
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;

    for &byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                if depth == stack.len() {
                    return false;
                }
                stack[depth] = byte;
                depth += 1;
            }
            b'}' | b']' => {
                let expected = if byte == b'}' { b'{' } else { b'[' };
                if depth == 0 || stack[depth - 1] != expected {
                    return false;
                }
                depth -= 1;
            }
            _ => {}
        }
    }

    true
}

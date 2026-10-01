//! A content proof, not a schema-version preference. V1 migration can skip rows,
//! so neither its completion marker nor session coverage proves a safe overlap.

use std::collections::BTreeSet;

use rusqlite::{Connection, Row, Statement};
use serde_json::Value;

use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

use super::super::{normalization::opencode_event_time, OpenCodeSqliteDialect};
use super::json::{project_json, retained_projection, unambiguous_object};
use super::model::OpenCodeNativeSchemaFamily;
use crate::provider::normalization::provider_required_timestamp_millis;

pub(super) fn legacy_is_covered(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<bool> {
    // Check every legacy relationship, including parts outside any joined message.
    if conn.query_row(
        "select exists(select 1 from part p left join message m on m.id=p.message_id collate binary
             where m.id is null or p.session_id is not m.session_id collate binary
                or typeof(p.id)<>'text' or trim(p.id)='' or octet_length(p.id)>4096
                or typeof(p.time_created)<>'integer' or typeof(p.time_updated)<>'integer')",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(false);
    }
    let session_table = if conn.query_row(
        "select exists(select 1 from sqlite_schema where type='table' and name='session_v2')",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        "session_v2"
    } else {
        "session"
    };
    let sql = format!(
        "select m.id, {}, n.type, {},
                n.id is not null and n.session_id is m.session_id collate binary and s.id is not null
                and typeof(m.id)='text' and trim(m.id)<>'' and octet_length(m.id)<=4096
                and typeof(m.session_id)='text' and trim(m.session_id)<>''
                and octet_length(m.session_id)<=4096
                and typeof(m.time_created)='integer' and typeof(m.time_updated)='integer'
                and (n.time_created is m.time_created or (n.type='compaction' and n.id<>m.id collate binary))
                and (n.id=m.id collate binary or exists(select 1 from message parent
                     where parent.id=n.id collate binary and parent.session_id=m.session_id collate binary)),
                n.time_created, m.session_id, n.id
         from message m left join session_message n on n.id=coalesce(
             (select id from session_message where id=m.id collate binary),
             case when json_valid(m.data) then
                 case when json_extract(m.data,'$.role')='assistant'
                           and json_extract(m.data,'$.summary')=1
                      then json_extract(m.data,'$.parentID') end end) collate binary
         left join {session_table} s on s.id=m.session_id collate binary",
        bounded_json("m.data"),
        bounded_json("n.data"),
    );
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut paired_summaries = BTreeSet::new();
    let mut parts = conn.prepare(&format!(
        "select {} from part where message_id=?1 collate binary order by id collate binary",
        bounded_json("data"),
    ))?;
    while let Some(row) = rows.next()? {
        if !row.get::<_, bool>(4)? {
            return Ok(false);
        }
        let (Some(legacy), Some(raw_current)) = (object(row, 1)?, row.get::<_, Option<String>>(3)?)
        else {
            return Ok(false);
        };
        let kind: String = row.get(2)?;
        // The covering message must survive the same JSON admission used by
        // the scanner; field equality alone cannot certify a rejected row.
        let projection = project_json(
            &raw_current,
            &kind,
            None,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
            dialect,
            &mut false,
        );
        let Some(current) = retained_projection(&projection) else {
            return Ok(false);
        };
        let current = current.body;
        if !current.is_object() {
            return Ok(false);
        }
        let created: i64 = row.get(5)?;
        let time = opencode_event_time(&current, dialect).and_then(|time| {
            time.map_or_else(
                || {
                    provider_required_timestamp_millis(
                        created,
                        dialect.session_message_time_created_field,
                    )
                },
                Ok,
            )
        });
        if !time.is_ok_and(|time| time.timestamp_millis() == created) {
            return Ok(false);
        }
        let role = legacy.get("role").and_then(Value::as_str);
        if !matches!(
            (role, kind.as_str()),
            (Some("user"), "user" | "synthetic" | "compaction")
                | (Some("assistant"), "assistant" | "compaction")
        ) || (kind != "compaction"
            && current
                .get("role")
                .is_some_and(|value| value.as_str() != role))
        {
            return Ok(false);
        }
        if kind == "compaction"
            && role == Some("assistant")
            && (legacy.get("summary") != Some(&Value::Bool(true))
                || legacy.pointer("/time/completed").is_none()
                || legacy.get("error").is_some()
                || legacy.get("parentID").and_then(Value::as_str)
                    != Some(row.get::<_, String>(7)?.as_str())
                || row.get::<_, String>(0)? == row.get::<_, String>(7)?
                || !paired_summaries.insert((row.get::<_, String>(6)?, row.get::<_, String>(7)?)))
        {
            return Ok(false);
        }
        let mut coverage = Coverage {
            kind: &kind,
            summary: kind == "compaction" && role == Some("assistant"),
            current: &current,
            text: String::new(),
            blocks: 0,
            step_start: None,
            snapshot_start: None,
            patch_start: None,
            step_end: None,
            cost: None,
            tokens: None,
            finish: None,
        };
        {
            let mut parts = parts.query([row.get::<_, String>(0)?])?;
            while let Some(part) = parts.next()? {
                let Some(part) = object(part, 0)? else {
                    return Ok(false);
                };
                if !coverage.part(&part) {
                    return Ok(false);
                }
            }
        }
        let text_key = if coverage.summary { "summary" } else { "text" };
        if ((kind != "assistant" && kind != "compaction") || coverage.summary)
            && current.get(text_key).and_then(Value::as_str) != Some(coverage.text.as_str())
        {
            return Ok(false);
        }
        if kind == "compaction"
            && (coverage.blocks == 0
                || current.get("status").and_then(Value::as_str) != Some("completed"))
        {
            return Ok(false);
        }
        if !coverage.metadata_covered() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn bounded_json(column: &str) -> String {
    format!("case when typeof({column})='text' and octet_length({column})<={MAX_PROVIDER_SQLITE_VALUE_BYTES} then {column} end")
}

fn object(row: &Row<'_>, column: usize) -> Result<Option<Value>> {
    Ok(row
        .get::<_, Option<String>>(column)?
        .as_deref()
        .and_then(unambiguous_object))
}

struct Coverage<'a> {
    kind: &'a str,
    summary: bool,
    current: &'a Value,
    text: String,
    blocks: usize,
    step_start: Option<Value>,
    snapshot_start: Option<Value>,
    patch_start: Option<Value>,
    step_end: Option<Value>,
    cost: Option<f64>,
    tokens: Option<Value>,
    finish: Option<Value>,
}

impl Coverage<'_> {
    fn part(&mut self, part: &Value) -> bool {
        let Some(kind) = part.get("type").and_then(Value::as_str) else {
            return false;
        };
        let fields: &[&str] = match kind {
            "text" => &["type", "text", "synthetic", "ignored", "metadata", "time"],
            "reasoning" => &["type", "text", "metadata", "time"],
            "tool" => &["type", "callID", "tool", "state", "metadata"],
            "step-start" | "snapshot" => &["type", "snapshot"],
            "step-finish" => &["type", "reason", "snapshot", "cost", "tokens"],
            "patch" => &["type", "hash", "files"],
            "compaction" => &["type", "auto"],
            _ => return false,
        };
        if !has_only_keys(part, fields)
            || part.get("metadata").is_some_and(|value| !value.is_object())
        {
            return false;
        }
        if self.kind == "compaction" && !self.summary {
            self.blocks += 1;
            return self.blocks == 1
                && kind == "compaction"
                && part.get("auto").is_some_and(Value::is_boolean)
                && self.current.get("reason").and_then(Value::as_str)
                    == Some(if part["auto"] == Value::Bool(true) {
                        "auto"
                    } else {
                        "manual"
                    });
        }
        if self.kind != "assistant" {
            if kind != "text"
                || part.get("ignored") == Some(&Value::Bool(true))
                || (!self.summary
                    && (part.get("synthetic") == Some(&Value::Bool(true)))
                        != (self.kind == "synthetic"))
            {
                return false;
            }
            // V2's user/summary concatenation has no per-part metadata carrier.
            if part
                .get("metadata")
                .and_then(Value::as_object)
                .is_some_and(|value| !value.is_empty())
                || !text_flags_and_time_covered(part, self.current)
            {
                return false;
            }
            let Some(text) = part.get("text").and_then(Value::as_str) else {
                return false;
            };
            if self.text.len().saturating_add(text.len()).saturating_add(2)
                > MAX_PROVIDER_SQLITE_VALUE_BYTES
            {
                return false;
            }
            if self.blocks > 0 {
                self.text.push_str("\n\n");
            }
            self.text.push_str(text);
            self.blocks += 1;
            return true;
        }
        match kind {
            "text" | "reasoning" | "tool" => {
                let Some(block) = self
                    .current
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|blocks| blocks.get(self.blocks))
                else {
                    return false;
                };
                self.blocks += 1;
                assistant_block_covered(part, block)
            }
            "step-start" | "snapshot" => {
                if part.get("snapshot").is_some_and(|value| !value.is_string())
                    || kind == "snapshot" && part.get("snapshot").is_none()
                {
                    return false;
                }
                let start = if kind == "step-start" {
                    &mut self.step_start
                } else {
                    &mut self.snapshot_start
                };
                if start.is_none() {
                    *start = part.get("snapshot").cloned();
                }
                true
            }
            "step-finish" => {
                if part.get("snapshot").is_some_and(|value| !value.is_string()) {
                    return false;
                }
                if let Some(snapshot) = part.get("snapshot") {
                    self.step_end = Some(snapshot.clone());
                }
                if let Some(cost) = part.get("cost") {
                    let Some(cost) = cost.as_f64() else {
                        return false;
                    };
                    self.cost = Some(self.cost.unwrap_or(0.0) + cost);
                }
                if let Some(tokens) = part.get("tokens") {
                    let Some(tokens) = migration_tokens(tokens) else {
                        return false;
                    };
                    self.tokens = Some(tokens);
                }
                if let Some(reason) = part.get("reason") {
                    if !matches!(
                        reason.as_str(),
                        Some(
                            "stop"
                                | "length"
                                | "tool-calls"
                                | "content-filter"
                                | "error"
                                | "unknown"
                        )
                    ) {
                        return false;
                    }
                    self.finish = Some(reason.clone());
                }
                true
            }
            "patch" => {
                let Some(files) = part.get("files").and_then(Value::as_array) else {
                    return false;
                };
                if !part.get("hash").is_some_and(Value::is_string)
                    || !files.iter().all(Value::is_string)
                {
                    return false;
                }
                if self.patch_start.is_none() {
                    self.patch_start = part.get("hash").cloned();
                }
                self.current
                    .pointer("/snapshot/files")
                    .and_then(Value::as_array)
                    .is_some_and(|current| files.iter().all(|file| current.contains(file)))
            }
            // Subtask/file and unknown mappings need their own lossless proof.
            _ => false,
        }
    }

    fn metadata_covered(&self) -> bool {
        // Native v1 accumulates step cost but retains the last step's tokens.
        // V2 stores first start / last end snapshots and a union of patch paths.
        optional_equal(
            self.step_start
                .as_ref()
                .or(self.snapshot_start.as_ref())
                .or(self.patch_start.as_ref()),
            self.current.pointer("/snapshot/start"),
        ) && optional_equal(
            self.step_end.as_ref(),
            self.current.pointer("/snapshot/end"),
        ) && self
            .cost
            .is_none_or(|cost| self.current.get("cost").and_then(Value::as_f64) == Some(cost))
            && optional_equal(self.tokens.as_ref(), self.current.get("tokens"))
            && optional_equal(self.finish.as_ref(), self.current.get("finish"))
    }
}

fn optional_equal(legacy: Option<&Value>, current: Option<&Value>) -> bool {
    legacy.is_none() || legacy == current
}

fn time_covered(legacy: &Value, current: &Value) -> bool {
    valid_part_time(legacy)
        && legacy
            .get("start")
            .is_some_and(|start| Some(start) == current.get("created"))
        && optional_equal(legacy.get("end"), current.get("completed"))
}

fn valid_part_time(legacy: &Value) -> bool {
    has_only_keys(legacy, &["start", "end"])
        && legacy
            .get("start")
            .is_some_and(|value| value.as_i64().is_some_and(|time| time >= 0))
        && legacy
            .get("end")
            .is_none_or(|value| value.as_i64().is_some_and(|time| time >= 0))
}

fn assistant_block_covered(part: &Value, block: &Value) -> bool {
    if part.get("type") != block.get("type") {
        return false;
    }
    if part.get("type").and_then(Value::as_str) == Some("tool") {
        return tool_covered(part, block);
    }
    part.get("text").is_some_and(Value::is_string)
        && part.get("text") == block.get("text")
        && optional_equal(part.get("metadata"), block.get("state"))
        && if part.get("type").and_then(Value::as_str) == Some("reasoning") {
            time_covered(&part["time"], &block["time"])
        } else {
            ["synthetic", "ignored"].into_iter().all(|key| {
                part.get(key)
                    .is_none_or(|value| value == &Value::Bool(false))
            }) && part.get("time").is_none_or(|time| {
                valid_part_time(time)
                    && block
                        .get("time")
                        .is_none_or(|current| time_covered(time, current))
            })
        }
}

fn tool_covered(part: &Value, block: &Value) -> bool {
    if !part.get("callID").is_some_and(Value::is_string)
        || part.get("callID") != block.get("id")
        || !part.get("tool").is_some_and(Value::is_string)
        || part.get("tool") != block.get("name")
        || !optional_equal(part.get("metadata"), block.get("providerState"))
    {
        return false;
    }
    let old = &part["state"];
    let new = &block["state"];
    if !has_only_keys(
        old,
        &[
            "status",
            "input",
            "output",
            "error",
            "title",
            "metadata",
            "time",
            "attachments",
        ],
    ) || !old.get("input").is_some_and(Value::is_object)
        || old.get("metadata").is_some_and(|value| !value.is_object())
        || old.get("title").is_some_and(|title| {
            !title.is_string()
                || match new.get("title") {
                    Some(current) => current != title,
                    // Native v2 omits completed-tool titles. The retained scan
                    // restores this field from the same proven legacy part.
                    None => old.get("status").and_then(Value::as_str) != Some("completed"),
                }
        })
        || old.get("input") != new.get("input")
        || !optional_equal(old.get("metadata"), new.get("metadata"))
    {
        return false;
    }
    match old.get("status").and_then(Value::as_str) {
        Some("completed") => {
            if new.get("status").and_then(Value::as_str) != Some("completed")
                || old.pointer("/time/compacted").is_some()
                || old.get("error").is_some()
                || old
                    .get("attachments")
                    .is_some_and(|value| !value.is_array())
                || !time_covered(&old["time"], &block["time"])
            {
                return false;
            }
            let Some(content) = new.get("content").and_then(Value::as_array) else {
                return false;
            };
            if !old.get("output").is_some_and(Value::is_string)
                || content
                    .first()
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    != Some("text")
                || old.get("output") != content.first().and_then(|item| item.get("text"))
            {
                return false;
            }
            let attachments = old
                .get("attachments")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            content.len() == attachments.len() + 1
                && attachments.iter().zip(&content[1..]).all(|(file, block)| {
                    has_only_keys(
                        file,
                        &[
                            "type",
                            "id",
                            "sessionID",
                            "messageID",
                            "url",
                            "mime",
                            "filename",
                        ],
                    ) && file.get("url").is_some_and(Value::is_string)
                        && file.get("mime").is_some_and(Value::is_string)
                        && block.get("type").and_then(Value::as_str) == Some("file")
                        && file.get("url") == block.get("uri")
                        && file.get("mime") == block.get("mime")
                        && optional_equal(file.get("filename"), block.get("name"))
                })
        }
        Some("error") => {
            new.get("status").and_then(Value::as_str) == Some("error")
                && old.get("output").is_none()
                && old.get("attachments").is_none()
                && old.get("error").is_some_and(Value::is_string)
                && old.get("error") == new.pointer("/error/message")
                && time_covered(&old["time"], &block["time"])
        }
        _ => false,
    }
}

pub(super) fn legacy_part_rows(conn: &Connection) -> Result<Statement<'_>> {
    Ok(conn.prepare(&format!(
        "select {} from part where message_id=?1 collate binary order by id collate binary",
        bounded_json("data"),
    ))?)
}

/// Only called after whole-overlap admission, against that same pinned snapshot.
/// Reuse the proof's block correspondence before restoring native fields dropped
/// by v2 migration: completed-tool titles and per-text-part timestamps.
/// Returns (carries proven legacy parts, restored fields).
pub(super) fn restore_assistant_fields(
    parts: &mut Statement<'_>,
    message: &str,
    current: &mut Value,
) -> Result<(bool, bool)> {
    let mut blocks = current
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten();
    let mut rows = parts.query([message])?;
    let mut covered = false;
    let mut restored = false;
    let mut restored_bytes = 0_usize;
    while let Some(row) = rows.next()? {
        covered = true;
        let part = object(row, 0)?.ok_or_else(|| {
            CaptureError::InvalidPayload("OpenCode proven legacy part is invalid".into())
        })?;
        let kind = part.get("type").and_then(Value::as_str);
        if !matches!(kind, Some("text" | "reasoning" | "tool")) {
            continue;
        }
        let block = blocks
            .next()
            .filter(|block| assistant_block_covered(&part, block))
            .ok_or_else(|| {
                CaptureError::InvalidPayload("OpenCode proven block correspondence changed".into())
            })?;
        if let Some(title) = part
            .pointer("/state/title")
            .and_then(Value::as_str)
            .filter(|_| block.pointer("/state/title").is_none())
        {
            restored_bytes =
                restored_bytes.saturating_add(part["state"]["title"].to_string().len());
            block["state"]["title"] = Value::String(title.to_owned());
            restored = true;
        }
        if let Some(time) = part
            .get("time")
            .filter(|_| kind == Some("text") && block.get("time").is_none())
        {
            let mut mapped = serde_json::json!({"created":time["start"]});
            if let Some(end) = time.get("end") {
                mapped["completed"] = end.clone();
            }
            restored_bytes = restored_bytes.saturating_add(mapped.to_string().len());
            block["time"] = mapped;
            restored = true;
        }
        if restored_bytes > ctx_history_core::MAX_CORE_CONTENT_BYTES {
            return Err(CaptureError::InvalidPayload(
                "OpenCode restored assistant fields exceed Core content limits".into(),
            ));
        }
    }
    Ok((covered, restored))
}

fn has_only_keys(value: &Value, fields: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| fields.contains(&key.as_str())))
}

fn text_flags_and_time_covered(part: &Value, current: &Value) -> bool {
    ["synthetic", "ignored"]
        .into_iter()
        .all(|key| part.get(key).is_none_or(Value::is_boolean))
        && part
            .get("time")
            .is_none_or(|time| time_covered(time, &current["time"]))
}

fn migration_tokens(tokens: &Value) -> Option<Value> {
    // V1 getUsage excludes reasoning/cache from output/input counters. V2
    // retains those five counters but omits total; omit it here only if exact.
    if !has_only_keys(tokens, &["total", "input", "output", "reasoning", "cache"])
        || tokens
            .get("cache")
            .is_some_and(|cache| !has_only_keys(cache, &["read", "write"]))
    {
        return None;
    }
    let pointers = [
        "/input",
        "/output",
        "/reasoning",
        "/cache/read",
        "/cache/write",
    ];
    if pointers.iter().any(|pointer| {
        tokens
            .pointer(pointer)
            .is_some_and(|value| value.as_u64().is_none())
    }) {
        return None;
    }
    if let Some(total) = tokens.get("total") {
        let reconstructed = pointers.iter().try_fold(0_u64, |sum, pointer| {
            sum.checked_add(tokens.pointer(pointer)?.as_u64()?)
        })?;
        if total.as_u64()? != reconstructed {
            return None;
        }
    }
    let mut normalized = tokens.as_object()?.clone();
    normalized.remove("total");
    Some(Value::Object(normalized))
}

#[cfg(test)]
#[path = "overlap_tests.rs"]
mod tests;

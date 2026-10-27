//! fx schema-v3 transaction authority is owned by ctx-history-provider-fx.
mod mapping;
mod privacy;
mod usage;
use crate::{
    capture,
    records::Writer,
    source::{self, Source},
    spool::Spool,
    Error, Provider, Result,
};
use ctx_history_provider_fx::{
    decode_authority, decode_first_event_binding, decode_watermark, replay_committed,
    BoundaryIntent, ColdReplayDisposition, ReplayLimits,
};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

fn native(error: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("fx transactional source: {error}"))
}
fn sidecars(dir: &Path, limit: usize) -> Result<(Value, Value, Value)> {
    for pending in ["authority.pending.json", "commit.pending.json"] {
        if dir.join(pending).exists() {
            return Err(native(
                "authority or commit transition is pending; retry after the native writer finishes",
            ));
        }
    }
    let a = capture::object(&dir.join("authority.json"), limit)?;
    let authority =
        decode_authority(&serde_json::to_vec(&a)?, ReplayLimits::default()).map_err(native)?;
    let mut reader = source::reader(&dir.join("events.jsonl"))?;
    let mut bytes = Vec::new();
    let row = source::line(&mut *reader, &mut bytes, limit)?
        .ok_or_else(|| native("missing initial event"))?;
    if row.1 {
        return Err(native("initial event exceeds record limit"));
    }
    let first = decode_first_event_binding(&bytes, ReplayLimits::default()).map_err(native)?;
    if first.native_session_id != authority.session_id {
        return Err(native("initial event disagrees with authority"));
    }
    let wm = capture::object(
        &dir.join(format!("commit.{}.json", first.log_generation)),
        limit,
    )?;
    let watermark =
        decode_watermark(&serde_json::to_vec(&wm)?, ReplayLimits::default()).map_err(native)?;
    if watermark.session_id != authority.session_id
        || watermark.log_generation != first.log_generation
    {
        return Err(native("watermark disagrees with authority"));
    }
    let first = source::decode(&bytes).ok_or_else(|| native("invalid first event"))?;
    Ok((a, wm, first))
}

pub(crate) fn inspect(dir: &Path, limit: usize) -> Result<Option<Source>> {
    if !dir.join("authority.json").is_file() {
        return Ok(None);
    }
    let (authority, watermark, first) = sidecars(dir, limit)?;
    let session = authority["session_id"]
        .as_str()
        .ok_or_else(|| native("missing session ID"))?
        .to_owned();
    let commit = format!(
        "commit.{}.json",
        watermark["log_generation"].as_str().unwrap_or_default()
    );
    let stamp = capture::stamp(
        dir,
        &[
            "authority.json",
            "events.jsonl",
            &commit,
            "authority.pending.json",
            "commit.pending.json",
        ],
    )?;
    let mut header = json!({"cwd":first["payload"]["workspace_root"]});
    if let Some(ts) = capture::millis(&first["payload"]["created_at_ms"]) {
        header["startedAt"] = json!(ts);
    }
    Ok(Some(Source {
        provider: Provider::Fx,
        path: dir.to_owned(),
        session: session.clone(),
        segment: session,
        header,
        stamp,
    }))
}

pub(crate) fn ingest(spool: &mut Spool, dir: &Path) -> Result<()> {
    let (a, wm, _) = sidecars(dir, spool.limit)?;
    let limits = ReplayLimits::default();
    let authority = decode_authority(&serde_json::to_vec(&a)?, limits).map_err(native)?;
    let watermark = decode_watermark(&serde_json::to_vec(&wm)?, limits).map_err(native)?;
    if watermark.through_event_log_bytes > limits.max_committed_bytes {
        return Err(native(
            "committed byte watermark exceeds native replay limit",
        ));
    }
    // Retain one immutable byte snapshot for both validation and projection. Never
    // reopen a racing native log between these two operations.
    let mut frozen = tempfile::tempfile_in(&spool.bundle)?;
    let copied = std::io::copy(
        &mut File::open(dir.join("events.jsonl"))?.take(watermark.through_event_log_bytes),
        &mut frozen,
    )?;
    if copied != watermark.through_event_log_bytes {
        return Err(native("committed log is shorter than watermark"));
    }
    use std::io::{Seek, SeekFrom};
    frozen.seek(SeekFrom::Start(0))?;
    let scratch = privacy::CaptureScratch::new(&spool.bundle)?;
    let replay = replay_committed(
        &authority,
        &watermark,
        &mut BufReader::new(&mut frozen),
        BoundaryIntent::Stable,
        &scratch,
        limits,
    )
    .map_err(native)?;
    let ColdReplayDisposition::Canonical(replay) = replay else {
        return Err(native("pending transaction"));
    };
    let (after_a, after_wm, _) = sidecars(dir, spool.limit)?;
    if a != after_a || wm != after_wm {
        return Err(native("commit changed during capture; retry"));
    }
    spool.source_bytes += copied;
    frozen.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(frozen);
    let mut bytes = Vec::new();
    spool
        .db
        .execute_batch("CREATE TABLE fx_turn_sources(n INTEGER PRIMARY KEY, key TEXT)")?;
    let mut appended = 0u64;
    while let Some((_, oversize)) = source::line(&mut reader, &mut bytes, spool.limit)? {
        if oversize {
            return Err(native("committed event exceeds configured record bound"));
        }
        let v = source::decode(&bytes).ok_or_else(|| native("invalid committed event"))?;
        let key = format!("event:{}:{}", v["log_generation"], v["event_id"]);
        if v["kind"] == "state_replacement_committed" {
            spool.db.execute("DELETE FROM fx_turn_sources", [])?;
            appended = 0;
        } else if v["kind"] == "history_turn_committed" {
            spool.db.execute(
                "INSERT INTO fx_turn_sources VALUES(?1,?2)",
                rusqlite::params![appended, key],
            )?;
            appended += 1;
        }
        spool.push(&json!({"component":"event","key":key,"row":v}), Some(&key))?;
    }
    // The reducer owns replacements and compaction. Current history is a snapshot,
    // not a fabricated set of transitions; the complete committed log stays audit data.
    let suffix_start = replay.state.history.len().saturating_sub(appended as usize);
    for (index, turn) in replay.state.history.iter().enumerate() {
        let turn = serde_json::to_value(turn)?;
        let key: Option<String> = if index >= suffix_start {
            use rusqlite::OptionalExtension;
            spool
                .db
                .query_row(
                    "SELECT key FROM fx_turn_sources WHERE n=?1",
                    [index - suffix_start],
                    |r| r.get(0),
                )
                .optional()?
        } else {
            None
        };
        let pos = spool.push(
            &json!({"component":"turn","index":index,"key":key,"row":turn}),
            None,
        )?;
        if let Some(id) = turn["tool_call"]["id"].as_str() {
            spool.db.execute(
                "INSERT INTO native_calls VALUES(?1,?2)",
                rusqlite::params![id, pos],
            )?;
        }
        for step in turn["execution"]["tool_steps"]
            .as_array()
            .into_iter()
            .flatten()
        {
            for call in step["tool_calls"].as_array().into_iter().flatten() {
                if let Some(id) = call["id"].as_str() {
                    spool.db.execute(
                        "INSERT INTO native_calls VALUES(?1,?2)",
                        rusqlite::params![id, pos],
                    )?;
                }
            }
            for result in step["tool_results"].as_array().into_iter().flatten() {
                if let Some(id) = result["tool_call_id"].as_str() {
                    spool.db.execute(
                        "INSERT INTO tool_results VALUES(?1,?2)",
                        rusqlite::params![id, pos],
                    )?;
                }
            }
        }
    }
    let mut state = replay.state;
    state.history.clear();
    spool.push(&json!({"component":"state","row":serde_json::to_value(state)?,"authority":a,"watermark":wm}),Some("state"))?;
    // Earlier replacements may contain marked answers no longer in current state.
    scratch.scan(spool)?;
    Ok(())
}

pub(crate) fn convert(spool: &mut Spool, w: &mut Writer, value: &Value) -> Result<()> {
    let n = &value["row"];
    match value["component"].as_str() {
        Some("event") => {
            let key = value["key"].as_str().unwrap();
            // Duplicate event IDs are not silently merged, even if the native
            // sequence validator accepts them.
            let unique = spool.unique(key)?;
            w.capture_identity(
                &if unique {
                    key.to_owned()
                } else {
                    format!("event-pos:{}", w.pos)
                },
                !unique,
                false,
            );
            let mut r = json!({"type":"com.vercel.fx/committed_event"});
            if let Some(ts) = capture::millis(&n["timestamp_ms"]) {
                r["ts"] = json!(ts);
                r["tsPrecision"] = json!("ms");
            }
            let mut retained = n.clone();
            if spool.has_secrets && retained["kind"] == "state_replacement_chunk" {
                retained["payload"]["base64"] =
                    json!("[secret-bearing replacement payload omitted]");
                crate::records::meta(&mut r,w.provider,"redaction",&json!("replacement bytes omitted because marked secret answers occur in the capture"));
            }
            capture::retain(&mut r, w.provider, &retained, spool)?;
            w.emit(r, &json!({}))?;
            mapping::event(spool, w, n)?;
        }
        Some("turn") => {
            let stable = value["key"]
                .as_str()
                .map(|key| spool.unique(key))
                .transpose()?
                .unwrap_or(false);
            let key = if stable {
                format!("history:{}", value["key"].as_str().unwrap())
            } else {
                format!("history:{}", value["index"])
            };
            w.capture_identity(&key, !stable, false);
            mapping::turn(spool, w, n)?;
        }
        _ => {
            w.capture_identity("state", false, true);
            mapping::state(spool, w, n)?;
            w.slot(100);
            let mut r = json!({"type":"com.vercel.fx/authority"});
            capture::retain(
                &mut r,
                w.provider,
                &json!({"authority":value["authority"],"watermark":value["watermark"]}),
                spool,
            )?;
            w.emit(r, &json!({}))?;
        }
    }
    Ok(())
}

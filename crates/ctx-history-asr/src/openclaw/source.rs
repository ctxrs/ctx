use super::{run, P};
use crate::{
    native as n,
    records::{copy, meta, session_ref},
    source::{decode, line, reader, Source},
    spool::Spool,
    Error, Input, Result,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{fs, io::Read, path::Path};

pub(crate) fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    let path = &input.path;
    if path.is_dir() {
        // A state root owns agent trees; exports/workspaces under the same root
        // are not transcript sources. Prefer the native database for an agent.
        let agents = path.join("agents");
        let database = path.join("agent/openclaw-agent.sqlite");
        if agents.is_dir() || database.is_file() {
            return visit(
                &Input {
                    provider: P,
                    path: if agents.is_dir() { agents } else { database },
                },
                limit,
                visitor,
                unidentified,
            );
        }
        for e in fs::read_dir(path)? {
            let e = e?;
            let p = e.path();
            if e.file_type()?.is_dir()
                || p.file_name().is_some_and(|s| s == "openclaw-agent.sqlite")
                || p.extension().is_some_and(|e| e == "jsonl")
            {
                visit(
                    &Input {
                        provider: P,
                        path: p,
                    },
                    limit,
                    visitor,
                    unidentified,
                )?;
            }
        }
        return Ok(());
    }
    if !path.is_file() {
        return Ok(());
    }
    if path.extension().is_some_and(|e| e == "jsonl") {
        let mut r = reader(path)?;
        let mut b = Vec::new();
        if let Some((_, false)) = line(&mut *r, &mut b, limit)? {
            if let Some(h) = decode(&b) {
                if h["type"] == "session" {
                    if let Some(id) = h["id"].as_str() {
                        return visitor(Source {
                            provider: P,
                            path: path.clone(),
                            session: id.into(),
                            header: json!({"legacy":true,"nativeHeader":h}),
                            stamp: String::new(),
                            segment: id.into(),
                        });
                    }
                }
            }
        }
        *unidentified += 1;
        return Ok(());
    }
    let db = n::open(path)?;
    if !n::has(&db, "session_windows")? || !n::has(&db, "transcript_events")? {
        *unidentified += 1;
        return Ok(());
    }
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if ![17, 19, 25, 37].contains(&version) {
        return Err(Error::Invalid(format!(
            "OpenClaw ASR supports schema 17, 19, 25 and 37; found {version}"
        )));
    }
    n::scan(
        &db,
        "SELECT session_id FROM session_windows ORDER BY session_id",
        [],
        limit,
        |row| {
            if let Some(id) = row["session_id"].as_str() {
                visitor(Source {
                    provider: P,
                    path: path.clone(),
                    session: id.into(),
                    header: json!({"schemaVersion":version.to_string()}),
                    stamp: String::new(),
                    segment: id.into(),
                })
            } else {
                *unidentified += 1;
                Ok(())
            }
        },
    )
}
pub(crate) fn ingest(
    s: &mut Spool,
    path: &Path,
    id: &str,
    h: &mut Value,
    bundle: &Connection,
) -> Result<()> {
    let mut native_header = Value::Null;
    if h["legacy"] == true {
        native_header = h["nativeHeader"].clone();
        let mut reader = reader(path)?;
        let mut b = Vec::new();
        let mut ordinal = 0;
        while let Some((_, over)) = line(&mut *reader, &mut b, s.limit)? {
            ordinal += 1;
            let e = if over {
                None
            } else {
                std::str::from_utf8(&b).ok().and_then(n::parse_json)
            };
            stage_event(s, e, ordinal, None, false)?;
        }
        n::loss(s, "_ai.openclaw/shared_state_unavailable")?;
        h["asr"] = json!({});
    } else {
        let db = n::open(path)?;
        let fields = n::columns(&db, "session_windows")?
            .into_iter()
            .filter(|k| k != "row_transcript_json")
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(",");
        let window = n::one(
            &db,
            &format!("SELECT {fields} FROM session_windows WHERE session_id=?1"),
            [id],
            s.limit,
        )?;
        if window.is_null() {
            return Err(Error::Invalid(
                "OpenClaw window disappeared during discovery".into(),
            ));
        }
        s.save("window", &window)?;
        let lane = window["session_key"].as_str().unwrap_or("");
        let node = if n::has(&db, "session_nodes")? {
            n::parse(
                &n::one(
                    &db,
                    "SELECT entry_json FROM session_nodes WHERE session_key=?1",
                    [lane],
                    s.limit,
                )?["entry_json"],
            )
        } else {
            Value::Null
        };
        let node = if node["sessionId"] == id {
            node
        } else {
            Value::Null
        };
        s.save("node", &node)?;
        if n::has(&db, "session_pending_inputs")? {
            n::scan(
                &db,
                "SELECT consumed_event_id,run_id FROM session_pending_inputs WHERE session_id=?1",
                [id],
                s.limit,
                |r| {
                    if let (Some(e), Some(run)) =
                        (r["consumed_event_id"].as_str(), r["run_id"].as_str())
                    {
                        s.save(&format!("input-run:{e}"), &json!(run))?;
                    }
                    Ok(())
                },
            )?;
        }
        let ancestor = window["previous_session_id"]
            .as_str()
            .or(node["forkSource"]["sessionId"].as_str());
        let identities = n::has(&db, "transcript_event_identities")?;
        if let Some(a) = ancestor.filter(|_| identities) {
            n::scan(
                &db,
                "SELECT event_id FROM transcript_event_identities WHERE session_id=?1",
                [a],
                s.limit,
                |r| {
                    if let Some(e) = r["event_id"].as_str() {
                        s.save(&format!("ancestor:{e}"), &json!(true))?;
                    }
                    Ok(())
                },
            )?;
        }
        let cols = n::columns(&db, "transcript_events")?;
        let compressed = cols.iter().any(|c| c == "event_zstd");
        let sql = if compressed {
            "SELECT seq,event_json,event_zstd,event_utf8_bytes FROM transcript_events WHERE session_id=?1 ORDER BY seq"
        } else {
            "SELECT seq,event_json,NULL,NULL FROM transcript_events WHERE session_id=?1 ORDER BY seq"
        };
        let mut stmt = db.prepare(sql)?;
        let mut rows = stmt.query([id])?;
        let mut prefix = ancestor.is_some();
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let raw = match row.get_ref(1)? {
                rusqlite::types::ValueRef::Text(bytes) if bytes.len() <= s.limit => {
                    Some(bytes.to_vec())
                }
                rusqlite::types::ValueRef::Null => {
                    let length: Option<i64> = row.get(3)?;
                    match (row.get_ref(2)?, length) {
                        (rusqlite::types::ValueRef::Blob(bytes), Some(len))
                            if len > 0 && (len as usize) <= s.limit && bytes.len() <= s.limit =>
                        {
                            let mut decoded = Vec::new();
                            let ok = zstd::stream::read::Decoder::new(bytes)
                                .and_then(|r| r.take(s.limit as u64 + 1).read_to_end(&mut decoded))
                                .is_ok();
                            (ok && decoded.len() == len as usize).then_some(decoded)
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let e = raw
                .and_then(|b| std::str::from_utf8(&b).ok().and_then(n::parse_json))
                .filter(Value::is_object);
            if e.as_ref().is_some_and(|e| e["type"] == "session") {
                native_header = e.clone().unwrap_or(Value::Null);
            }
            let copied = if let Some(e) = e.as_ref().and_then(|e| e["id"].as_str()) {
                s.entity(&format!("ancestor:{e}"))?.is_some()
            } else {
                false
            };
            if e.as_ref().is_some_and(|e| e["type"] != "session") && !copied {
                prefix = false;
            }
            stage_event(s, e, seq, ancestor.filter(|_| copied), prefix && copied)?;
        }
        let mut asr = json!({"conversationKey":lane});
        n::set_time(&mut asr, "startedAt", &window["started_at"]);
        let mut lineage = json!({});
        if let Some(a) = ancestor {
            if window["reason"] == "fork" {
                lineage["forkedFrom"] =
                    json!({"session":session_ref(P,a),"evidence":"native_field"});
                if let Some(e) = node["forkSource"]["entryId"].as_str() {
                    lineage["forkedFrom"]["record"] = json!(format!("messageId={e}"));
                }
            } else if let Some(reason) = window["reason"].as_str().filter(|x| {
                [
                    "reset",
                    "rollover",
                    "rewind",
                    "switch",
                    "recovery",
                    "compaction",
                    "daily",
                    "idle",
                ]
                .contains(x)
            }) {
                lineage["continues"] =
                    json!({"session":session_ref(P,a),"reason":reason,"evidence":"native_field"});
            }
            if s.entity("copied-prefix")?.is_some() {
                lineage["inherits"] =
                    json!({"mode":"copied","from":session_ref(P,a),"evidence":"native_structure"});
            }
        } else if window["reason"] == "initial"
            && window["parent_session_key"].is_null()
            && window["spawned_by"].is_null()
        {
            lineage = json!({"root":true,"inherits":{"mode":"none","evidence":"native_structure"}});
        }
        if lineage.as_object().is_some_and(|o| !o.is_empty()) {
            asr["lineage"] = lineage;
        }
        let agent_id = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|s| s.to_str());
        if let Some(agent) = agent_id {
            meta(&mut asr, P, "agentId", &json!(agent));
        }
        meta(&mut asr, P, "harnessId", &window["agent_harness_id"]);
        if n::has(&db, "session_conversations")? && n::has(&db, "conversations")? {
            let mut cs = Vec::new();
            n::scan(&db,"SELECT c.*,sc.role FROM conversations c JOIN session_conversations sc USING(conversation_id) WHERE sc.session_id=?1 ORDER BY c.conversation_id",[id],s.limit,|r|{let mut c=json!({"id":r["conversation_id"],"channel":r["channel"]});copy(&mut c,"kind",&r,"kind");copy(&mut c,"nativeId",&r,"peer_id");meta(&mut c,P,"role",&r["role"]);n::retain(&mut c,P,"row",&r);cs.push(c);Ok(())})?;
            if !cs.is_empty() {
                asr["conversations"] = json!(cs);
            }
        }
        h["asr"] = asr;
        super::side::ingest(s, &db, path, id, (lane, agent_id.unwrap_or("")), h, bundle)?;
        n::stage(s, "session_nodes", lane, node, true)?;
        n::stage(s, "session_windows", id, window, true)?;
        if n::has(&db, "session_transcript_index_state")? {
            let leaf = n::one(
                &db,
                "SELECT leaf_event_id FROM session_transcript_index_state WHERE session_id=?1",
                [id],
                s.limit,
            )?;
            n::stage(s, "head", id, leaf, true)?;
        }
    }
    if let Some(cwd) = native_header["cwd"].as_str().filter(|p| p.starts_with('/')) {
        h["asr"]["workspace"] = json!({"roots":[{"id":"workspace","path":cwd}]});
    }
    n::set_time(&mut h["asr"], "startedAt", &native_header["timestamp"]);
    if let Some(version) = native_header.get("version") {
        meta(&mut h["asr"], P, "transcriptVersion", version);
    }
    n::loss(s, "_ai.openclaw/request_context")?;
    n::loss(s, "_ai.openclaw/unexported_side_tables")?;
    prepare(s, h)
}
fn stage_event(
    s: &mut Spool,
    e: Option<Value>,
    seq: i64,
    ancestor: Option<&str>,
    prefix: bool,
) -> Result<()> {
    let Some(e) = e else {
        return n::stage(
            s,
            "_gap",
            &seq.to_string(),
            json!({"reason":"unparseable"}),
            false,
        );
    };
    let key = e["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("position:{seq}"));
    if e["id"].is_string() {
        let seen = format!("entry-id:{key}");
        if s.entity(&seen)?.is_some() {
            return Err(Error::Invalid("duplicate OpenClaw entry identity".into()));
        }
        s.save(&seen, &json!(true))?;
    }
    if let Some(run) = run(&e) {
        if s.entity(&format!("owned-run:{run}"))?.is_none()
            && s.entity(&format!("seen-run:{run}"))?.is_none()
        {
            s.save(&format!("seen-run:{run}"), &json!(true))?;
            if ancestor.is_none() {
                s.save(&format!("owned-run:{run}"), &json!(true))?;
            }
        }
    }
    crate::privacy::questions(s, &e)?;
    let mut v = json!({"table":"transcript_events","identity":format!("event:{key}"),"row":e,"snapshot":false});
    if key.starts_with("position:") {
        v["positional"] = json!(true);
    }
    if let Some(a) = ancestor {
        v["copiedFrom"] = json!(session_ref(P, a));
    }
    if prefix {
        v["copiedPrefix"] = json!(true);
        s.save("copied-prefix", &json!(true))?;
    }
    s.push_native(&v)
}
fn prepare(s: &mut Spool, h: &mut Value) -> Result<()> {
    let mut pos = 0;
    let mut first_message = true;
    let mut whole_mirror = true;
    while let Some(next) = s.next(pos)? {
        pos = next;
        let v = s.get(pos)?;
        if v["table"] != "transcript_events" {
            if !matches!(
                v["table"].as_str(),
                Some("session_nodes" | "session_windows" | "head")
            ) {
                whole_mirror = false;
            }
            if v["table"] == "session_nodes"
                && [
                    "goal",
                    "permissionMode",
                    "sandboxMode",
                    "queueMode",
                    "sendPolicy",
                    "elevatedLevel",
                    "ttsAuto",
                ]
                .iter()
                .any(|k| v["row"].get(*k).is_some())
            {
                whole_mirror = false;
            }
            continue;
        }
        let e = &v["row"];
        if e["type"] != "session"
            && e["type"] != "leaf"
            && (e["type"] != "message" || e["message"]["__openclaw"]["mirrorIdentity"].is_null())
        {
            whole_mirror = false;
        }
        if first_message && e["type"] == "message" {
            first_message = false;
            let window = s.entity("window")?.unwrap_or(Value::Null);
            let node = s.entity("node")?.unwrap_or(Value::Null);
            if node["createdVia"] == "cron" || window["hook_external_content_source"].is_string() {
                if let Some(trigger) = super::messages::trigger(s, e, h)? {
                    h["asr"]["trigger"] = trigger;
                }
            }
        }
        if let Some(run) = run(e) {
            if let Some(t) = super::messages::trigger(s, e, h)? {
                if s.entity(&format!("trigger:{run}"))?.is_none() {
                    s.save(&format!("trigger:{run}"), &t)?;
                }
            }
        }
        super::tools::index(s, e, pos)?;
    }
    if whole_mirror
        && s.entity("window")?
            .is_some_and(|w| w["agent_harness_id"] == "codex")
        && h["asr"]["lineage"]["executedBy"]
            .as_array()
            .is_some_and(|a| {
                a.len() == 1
                    && a[0]["session"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("codex:"))
            })
    {
        h["asr"]["authority"] = json!("mirror");
    }
    Ok(())
}

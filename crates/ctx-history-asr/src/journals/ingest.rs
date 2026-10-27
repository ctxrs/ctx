use crate::{source, spool::Spool, Provider, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    io::{Seek, Write},
    path::Path,
};

pub(crate) fn ingest(
    spool: &mut Spool,
    path: &Path,
    provider: Provider,
    segment: &str,
) -> Result<()> {
    initialize(spool)?;
    let mut bytes = Vec::new();
    let mut pos = 0u64;
    let mut turn = 0u64;
    let archive = path.with_file_name("chat-archive.jsonl");
    let active = path.with_file_name("chat.jsonl");
    let files = if provider == Provider::Mux {
        [archive.as_path(), active.as_path()]
            .into_iter()
            .filter(|p| p.is_file())
            .collect()
    } else {
        vec![path]
    };
    for file in files {
        let mut reader = source::reader(file)?;
        while let Some((size, oversized)) = source::line(&mut *reader, &mut bytes, spool.limit)? {
            pos += 1;
            spool.source_bytes += size as u64;
            if bytes.iter().all(u8::is_ascii_whitespace) && !oversized {
                continue;
            }
            let mut v = (!oversized).then(|| decode(&bytes)).flatten().unwrap_or_else(|| json!({"type":"_gap","reason":if oversized {"conversion_loss"} else {"unparseable"}}));
            if provider == Provider::Junie {
                if v["kind"] == "UserPromptEvent" {
                    turn = pos;
                }
                if let (Some(kind), Some(step)) = (
                    v.pointer("/event/agentEvent/kind").and_then(Value::as_str),
                    v.pointer("/event/agentEvent/stepId")
                        .and_then(Value::as_str),
                ) {
                    let key = format!("junie:{turn}:{kind}:{step}");
                    v["_asr_step_key"] = json!(key);
                    let mut folded = spool.entity(&key)?.unwrap_or(v.clone());
                    if let Some(fields) = v["event"]["agentEvent"].as_object() {
                        for (k, value) in fields {
                            if !value.is_null() && !value.as_str().is_some_and(str::is_empty) {
                                folded["event"]["agentEvent"][k] = value.clone();
                            }
                        }
                    }
                    folded["_asr_step_key"] = json!(key);
                    if folded.get("_asr_folded").is_none() {
                        folded["_asr_folded"] = json!([]);
                    }
                    if let Some(a) = folded["_asr_folded"].as_array_mut() {
                        a.push(json!(format!("line:{pos}")));
                    }
                    if let Some(ts) = v.get("timestampMs") {
                        folded["timestampMs"] = ts.clone();
                    }
                    spool.save(&key, &folded)?;
                    spool.db.execute("INSERT INTO journal_messages VALUES(?1,?2,?2) ON CONFLICT(id) DO UPDATE SET last=excluded.last",params![key,pos])?;
                }
            }
            insert(spool, pos, &v, segment)?;
            if provider == Provider::Tabnine {
                if let Some(id) = v["id"].as_str() {
                    spool.db.execute("INSERT INTO journal_messages VALUES(?1,?2,?2) ON CONFLICT(id) DO UPDATE SET last=excluded.last",params![id,pos])?;
                }
            }
        }
    }
    if provider == Provider::Mux {
        if let Some(partial) =
            super::source::document(&path.with_file_name("partial.json"), spool.limit)?
        {
            spool.save("journal:partial", &partial)?;
        }
    }
    // Index only effective calls. Repeated snapshots are not repeated invocations.
    let mut p = 0;
    let mut partial_existing = false;
    let mut partial_selected = false;
    while let Some(next) = spool.next(p)? {
        p = next;
        if provider == Provider::Tabnine && !effective(spool, p)? {
            continue;
        }
        let v = spool.get(p)?;
        if provider == Provider::Mux {
            if let Some(partial) = spool.entity("journal:partial")? {
                if partial
                    .pointer("/metadata/historySequence")
                    .is_some_and(|s| {
                        !s.is_null() && Some(s) == v.pointer("/metadata/historySequence")
                    })
                {
                    partial_existing = true;
                }
            }
            if super::mux::selected_partial(spool, &v)? {
                partial_selected = true;
                continue;
            }
        }
        for (n, id) in call_ids(provider, &v).iter().enumerate() {
            spool.db.execute(
                "INSERT INTO native_calls VALUES(?1,?2)",
                params![id, p * crate::records::STRIDE + n as u64],
            )?;
        }
    }
    if provider == Provider::Mux {
        let selected = !partial_existing || partial_selected;
        spool.save("journal:partial-effective", &json!(selected))?;
        if let Some(partial) = spool.entity("journal:partial")?.filter(|_| selected) {
            for (n, id) in call_ids(provider, &partial).iter().enumerate() {
                spool.db.execute(
                    "INSERT INTO native_calls VALUES(?1,?2)",
                    params![id, (pos + 1) * crate::records::STRIDE + n as u64],
                )?;
            }
        }
    }
    spool.bytes.flush()?;
    Ok(())
}
pub(crate) fn initialize(spool: &Spool) -> Result<()> {
    spool.db.execute_batch("CREATE TABLE IF NOT EXISTS journal_messages(id TEXT PRIMARY KEY,first INTEGER,last INTEGER)")?;
    Ok(())
}
fn insert(spool: &mut Spool, pos: u64, v: &Value, segment: &str) -> Result<()> {
    initialize(spool)?;
    crate::privacy::questions(spool, v)?;
    let off = spool.bytes.stream_position()?;
    let bytes = serde_json::to_vec(v)?;
    spool.bytes.write_all(&bytes)?;
    spool.db.execute("INSERT INTO rows(pos,off,len,kind,native_id,segment,observed) VALUES(?1,?2,?3,?4,?5,?6,?1)",params![pos,off,bytes.len(),v["type"].as_str().or(v["kind"].as_str()),v["id"].as_str().or(v["message_id"].as_str()),segment])?;
    Ok(())
}
pub(crate) fn effective(spool: &Spool, pos: u64) -> Result<bool> {
    let overwritten: bool = spool.db.query_row("SELECT EXISTS(SELECT 1 FROM rows r JOIN journal_messages m ON r.native_id=m.id WHERE r.pos=?1 AND r.pos!=m.last)", [pos], |r|r.get(0))?;
    Ok(!overwritten)
}
pub(crate) fn folded(spool: &mut Spool, pos: u64, value: &Value) -> Result<Option<Value>> {
    let Some(id) = value["id"].as_str().or(value["_asr_step_key"].as_str()) else {
        return Ok(Some(value.clone()));
    };
    let row: Option<(u64, u64)> = spool
        .db
        .query_row(
            "SELECT first,last FROM journal_messages WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((first, last)) if first == pos => {
            if value.get("_asr_step_key").is_some() {
                let mut value = spool.entity(id)?;
                if let Some(v) = &mut value {
                    crate::privacy::sanitize(spool, v)?;
                }
                return Ok(value);
            }
            let mut value = spool.get(last)?;
            if first != last {
                value["_asr_folded"] = json!([format!("line:{first}"), format!("line:{last}")]);
            }
            Ok(Some(value))
        }
        Some(_) => Ok(None),
        None => Ok(Some(value.clone())),
    }
}
fn call_ids(provider: Provider, v: &Value) -> Vec<String> {
    let (array, key) = match provider {
        Provider::MistralVibe => (&v["tool_calls"], "id"),
        Provider::Tabnine => (&v["toolCalls"], "id"),
        Provider::Mux => (&v["parts"], "toolCallId"),
        Provider::Cursor => (&v["message"]["content"], "id"),
        _ => return Vec::new(),
    };
    let mut ids: Vec<String> = array
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| match provider {
            Provider::Cursor => p["type"] == "tool_use" && v["role"] == "assistant",
            Provider::Mux => p["type"] == "dynamic-tool",
            _ => true,
        })
        .flat_map(|p| {
            let mut ids: Vec<String> = p[key].as_str().map(str::to_owned).into_iter().collect();
            if provider == Provider::Mux {
                ids.extend(
                    p["nestedCalls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| p["toolCallId"].as_str().map(str::to_owned)),
                );
            }
            ids
        })
        .collect();
    if provider == Provider::MistralVibe {
        ids.extend(
            v.pointer("/manual_shell/operation_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        );
    }
    ids
}

pub(super) fn decode(bytes: &[u8]) -> Option<Value> {
    source::decode(bytes)
}

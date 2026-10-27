//! Native journal mappings. Similar-looking JSON from different providers is
//! intentionally dispatched to distinct semantic adapters.
mod common;
mod cursor;
pub(crate) mod ingest;
mod junie;
mod mux;
pub(crate) mod source;
mod tabnine;
mod tail;
mod vibe;
use crate::{records::Writer, spool::Spool, Provider, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
pub(crate) use tail::tail;

pub(crate) fn header(provider: Provider, id: &str, n: &Value) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":provider.agent()},"sessionId":id,"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":"ctx-asr-batch4-1","source":{"format":provider.format()},"lossy":["order","revision_history","message.author","tool_result.content","inference","cost","provider.identity","lineage","copies","_rs.ctx/native_line_limit","_rs.ctx/vendor_only"]},"profiles":["core","coding","usage","lineage"],"contentLevel":"full","workspace":{"roots":[{"id":"unknown","path":null}]}});
    let m = &n["metadata"];
    let start = match provider {
        Provider::Junie => common::millis(&m["createdAt"]),
        Provider::MistralVibe => m["start_time"].as_str().and_then(crate::records::timestamp),
        Provider::Tabnine => m["startTime"].as_str().and_then(crate::records::timestamp),
        _ => None,
    };
    if let Some(start) = start {
        h["startedAt"] = json!(start);
    }
    if provider == Provider::Tabnine {
        if let Some(a) = h["producer"]["lossy"].as_array_mut() {
            a.push(json!("_com.tabnine.cli/checkpoints"));
        }
    }
    if provider == Provider::Cursor {
        if let Some(a) = h["producer"]["lossy"].as_array_mut() {
            a.extend([
                json!("usage"),
                json!("timestamps"),
                json!("call_ids"),
                json!("reasoning"),
                json!("_com.cursor.agent/ide_and_blob_stores"),
                json!("_com.cursor.agent/external_tool_files"),
            ]);
        }
    }
    if provider == Provider::MistralVibe {
        if let Some(parent) = m["parent_session_id"]
            .as_str()
            .filter(|p| *p != id && !p.is_empty())
        {
            h["lineage"] = json!({"relatedUnknown":{"session":crate::records::session_ref(provider,parent),"evidence":"native_field"}});
        }
    }
    if let Some(parent) = n["parent"].as_str().filter(|p| *p != id) {
        h["scope"] = json!("subagent");
        h["lineage"] = json!({"spawnedBy":{"session":crate::records::session_ref(provider,parent),"evidence":"native_structure"}});
    }
    if let Some(owner) = n["archiveOwner"].as_str().filter(|p| *p != id) {
        h["scope"] = json!("subagent");
        h["lineage"] = json!({"relatedUnknown":{"session":crate::records::session_ref(provider,owner),"evidence":"native_structure"}});
    }
    h
}
pub(crate) fn convert(spool: &mut Spool, w: &mut Writer, input: &Value) -> Result<()> {
    let folded;
    let v = if matches!(w.provider, Provider::Tabnine | Provider::Junie) {
        folded = ingest::folded(spool, w.pos, input)?;
        let Some(ref value) = folded else {
            return Ok(());
        };
        value
    } else {
        input
    };
    let native_id = match w.provider {
        Provider::MistralVibe => v["message_id"].as_str(),
        Provider::Tabnine | Provider::Mux => v["id"].as_str(),
        _ => None,
    };
    let unique = if let Some(id) = native_id {
        if w.provider == Provider::Mux {
            mux::unique_message_id(spool, id)?
        } else {
            w.provider == Provider::Tabnine
                || spool.db.query_row(
                    "SELECT count(*)=1 FROM rows WHERE native_id=?1",
                    [id],
                    |r| r.get::<_, bool>(0),
                )?
        }
    } else {
        false
    };
    w.capture_scope = !unique;
    if unique {
        w.identity = native_id.map(|id| format!("j{:x}", Sha256::digest(id.as_bytes())));
    }
    if v["type"] == "_gap" {
        w.emit(json!({"type":"gap","reason":v["reason"]}), &json!({}))?;
        return Ok(());
    }
    match w.provider {
        Provider::Junie => junie::convert(spool, w, v),
        Provider::MistralVibe => vibe::convert(spool, w, v),
        Provider::Tabnine => tabnine::convert(spool, w, v),
        Provider::Mux => mux::convert(spool, w, v, false),
        Provider::Cursor => cursor::convert(spool, w, v),
        _ => Ok(()),
    }
}

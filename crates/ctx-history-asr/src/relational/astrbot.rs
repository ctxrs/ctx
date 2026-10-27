//! AstrBot data_v4.db at AstrBotDevs/AstrBot@c4616578d610c237766a5d497f9db4b607d1c75b.
use super::{common as c, sqlite};
use crate::{records::Writer, spool::Spool, Provider, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
const P: Provider = Provider::AstrBot;
pub(super) fn ingest(db: &Connection, s: &mut Spool, h: &mut Value) -> Result<()> {
    let id = h["_sessionId"].as_str().unwrap_or("").to_owned();
    let row = sqlite::one(db, "conversations", "conversation_id", &id, &[], s.limit)?;
    if let Some(raw) = row["content"].as_str() {
        if let Some(history) = c::parse(raw).filter(Value::is_array) {
            let mut counts = std::collections::BTreeMap::<&str, usize>::new();
            for n in history.as_array().unwrap() {
                if let Some(id) = n["id"].as_str().or_else(|| n["message_id"].as_str()) {
                    *counts.entry(id).or_default() += 1;
                }
            }
            for (i, n) in history.as_array().unwrap().iter().enumerate() {
                c::index_calls(s, &n["tool_calls"])?;
                if n["role"] == "tool" {
                    c::index_result(s, n["tool_call_id"].as_str())?;
                }
                let native = n["id"]
                    .as_str()
                    .or_else(|| n["message_id"].as_str())
                    .filter(|id| counts.get(id) == Some(&1));
                let key = native
                    .map(|id| format!("message:{id}"))
                    .unwrap_or_else(|| format!("content:{i}"));
                s.push_native("message", &key, n, native.is_none())?;
            }
        } else {
            s.push_native(
                "gap",
                "content",
                &json!({"reason":"unparseable","contentJsonText":raw}),
                true,
            )?;
        }
    } else if row.get("_oversized").is_some() {
        s.push_native(
            "gap",
            "content",
            &json!({"reason":"conversion_loss","sizes":row["_oversized"]}),
            true,
        )?;
    }
    if sqlite::columns(db, "provider_stats")?.contains("conversation_id") {
        sqlite::visit(
            db,
            "provider_stats",
            Some(("conversation_id", &id)),
            "id",
            &[],
            s.limit,
            &mut |n| {
                s.push_native(
                    "provider_stat",
                    &format!("stat:{}", c::id(&n["id"]).unwrap_or_default()),
                    &n,
                    false,
                )
            },
        )?;
    }
    s.push_native("session", "session", h, false)
}
pub(super) fn header(h: &Value, v: &mut Value) {
    if let Some(t) = c::time(&h["created_at"]) {
        v["startedAt"] = json!(t);
    }
    // platform_id is a configured adapter ID, not an ASR channel name.
    c::extra(
        v,
        P,
        "route",
        &json!({"platform_id":h["platform_id"],"user_id":h["user_id"]}),
    );
    c::extra(v, P, "persona", &h["persona_id"]);
    for loss in [
        "timestamps",
        "display_content",
        "_app.astrbot/unassigned_facts",
    ] {
        v["producer"]["lossy"]
            .as_array_mut()
            .unwrap()
            .push(json!(loss));
    }
}
pub(super) fn convert(s: &mut Spool, w: &mut Writer, _h: &Value, row: &Value) -> Result<()> {
    let n = &row["data"];
    match row["kind"].as_str().unwrap_or("") {
        "session" => {
            c::session_info(s, w, n, "title")?;
            // token_usage is retained context size, not lifetime token consumption.
            if let Some(tokens) = n["token_usage"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= c::SAFE)
            {
                c::emit(
                    s,
                    w,
                    json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"contextWindow":{"used":tokens}}),
                    &json!({"token_usage":tokens}),
                )?;
            }
            Ok(())
        }
        "provider_stat" => {
            let source = json!({"input_tokens":n["token_input_other"],"cache_read_tokens":n["token_input_cached"],"output_tokens":n["token_output"]});
            let v = json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":null,"counter":row["key"],"coverage":"partial","usage":c::usage(&source,true)});
            c::emit(s, w, v, n)?;
            Ok(())
        }
        "gap" => {
            c::emit(s, w, json!({"type":"gap","reason":n["reason"]}), n)?;
            Ok(())
        }
        _ => {
            let role = n["role"].as_str().unwrap_or("");
            if role == "_checkpoint" {
                // A linking marker, stripped before model replay; not a coding checkpoint.
                return c::vendor(s, w, "checkpoint", n);
            }
            let id = n["id"]
                .as_str()
                .or_else(|| n["message_id"].as_str())
                .filter(|_| !w.capture_scope)
                .map(str::to_owned)
                .unwrap_or_else(|| w.id());
            let mut v = if role == "tool" {
                c::result(s, w, n["tool_call_id"].as_str(), &n["content"])?
            } else if matches!(role, "assistant" | "user" | "system" | "developer") {
                c::message(s, w, &id, role, &n["content"])?
            } else {
                return c::vendor(s, w, "message", n);
            };
            if v["type"] == "message" && w.capture_scope {
                v["idSource"] = json!("derived");
            }
            if role == "assistant" {
                v["author"] = json!("model");
            }
            c::emit(s, w, v, n)?;
            for call in n["tool_calls"].as_array().into_iter().flatten() {
                c::call(
                    s,
                    w,
                    call,
                    call["id"].as_str(),
                    call["function"]["name"].as_str(),
                    &call["function"]["arguments"],
                )?;
            }
            Ok(())
        }
    }
}

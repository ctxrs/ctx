use crate::{
    capture, content,
    records::{copy, meta, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};

pub(super) fn convert(spool: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    // Native condense/truncation annotations are retained. No membership or
    // active-context claim is inferred from display order.
    let blocks = n["content"].as_array().filter(|blocks| {
        blocks.iter().any(|b| {
            matches!(
                b["type"].as_str(),
                Some("tool_use" | "server_tool_use" | "tool_result")
            )
        })
    });
    if let Some(blocks) = blocks {
        let basis = w.id();
        let scoped = w.capture_scoped();
        for (index, block) in blocks.iter().enumerate() {
            w.slot(index as u64 * 10);
            w.capture_identity(&format!("{basis}:block:{index}"), scoped, true);
            let (table, id, tag) = match block["type"].as_str() {
                Some("tool_use" | "server_tool_use") => {
                    ("native_calls", block["id"].as_str(), "call")
                }
                Some("tool_result") => ("tool_results", block["tool_use_id"].as_str(), "result"),
                _ => ("native_calls", None, "message"),
            };
            if let Some(id) = id {
                let count: u64 = spool.db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE call_id=?1"),
                    [id],
                    |r| r.get(0),
                )?;
                let calls: u64 = spool.db.query_row(
                    "SELECT count(*) FROM native_calls WHERE call_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                if count == 1 && calls == 1 {
                    w.capture_identity(&format!("{tag}:{id}"), false, true);
                }
            }
            match block["type"].as_str() {
                Some("tool_use" | "server_tool_use") => {
                    let mut r = json!({"type":"tool_call","requestedBy":"model"});
                    if let Some(id) = block["id"].as_str() {
                        r["callId"] = json!(id);
                        r["callIdSource"] = json!("native");
                    } else {
                        r["callId"] = json!(w.id());
                        r["callIdSource"] = json!("derived");
                    }
                    copy(&mut r, "name", block, "name");
                    if r.get("name").is_none() {
                        r["capture"] = json!({"name":{"state":"absent"}});
                    }
                    if let Some(input) = block.get("input") {
                        r[if capture::unsafe_numbers(input) {
                            "inputText"
                        } else {
                            "input"
                        }] = if capture::unsafe_numbers(input) {
                            json!(ctx_history_asr_core::numbers::text(input))
                        } else {
                            input.clone()
                        };
                    }
                    super::emit(spool, w, r, n)?;
                }
                Some("tool_result") => {
                    let mut r = json!({"type":"tool_result","linkage":"none","status":"unknown","content":content::parts(&block["content"],w.provider)});
                    if let Some(id) = block["tool_use_id"].as_str() {
                        // Count occurrences, including repeated calls within one array item.
                        let count: u64 = spool.db.query_row(
                            "SELECT count(*) FROM native_calls WHERE call_id=?1",
                            [id],
                            |r| r.get(0),
                        )?;
                        if count == 1 {
                            r["callId"] = json!(id);
                            r["linkage"] = json!("exact");
                        } else {
                            r["linkage"] = json!(if count > 1 { "ambiguous" } else { "none" });
                            meta(&mut r, w.provider, "nativeCallId", &json!(id));
                        }
                    }
                    if let Some(error) = block["is_error"].as_bool() {
                        r["status"] = json!(if error { "failed" } else { "completed" });
                    }
                    if block.get("content").is_none() {
                        r["capture"] = json!({"content":{"state":"absent"}});
                    }
                    super::emit(spool, w, r, n)?;
                }
                _ => message(spool, w, n, &json!([block]), false)?,
            }
        }
    } else {
        message(spool, w, n, &n["content"], true)?;
    }
    Ok(())
}
fn message(spool: &Spool, w: &mut Writer, n: &Value, body: &Value, whole: bool) -> Result<()> {
    // Block-level derived IDs avoid assigning the same native message entity to
    // several split context records. Original message ID remains in source metadata.
    let mut r = json!({"type":"message","messageId":w.id(),"idSource":"derived","audience":"model","content":content::parts(body,w.provider)});
    if whole {
        if let Some(id) = n["id"].as_str() {
            r["messageId"] = json!(id);
            r["idSource"] = json!("native");
        }
    }
    if let Some(role) = n["role"]
        .as_str()
        .filter(|s| matches!(*s, "user" | "assistant" | "system" | "developer"))
    {
        r["role"] = json!(role);
    } else {
        r["capture"] = json!({"role":{"state":"absent"}});
    }
    if body.is_null() {
        r["capture"]["content"] = json!({"state":"absent"});
    }
    // The source retains these reasoning representations outside content.
    if let Some(text) = n["reasoning_content"].as_str() {
        r["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"reasoning","text":text}));
    }
    super::emit(spool, w, r, n)
}

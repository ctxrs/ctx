//! OpenCode-family native projections, shared only for verified storage shapes.
mod content;
mod tools;
use crate::{
    opencode_source::millis,
    records::{copy, meta, Writer},
    spool::Spool,
    Result,
};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

pub(crate) fn convert(s: &mut Spool, w: &mut Writer, n: &Value, cwd: Option<&str>) -> Result<()> {
    let d = &n["data"];
    match n["type"].as_str().unwrap_or("") {
        "_gap" => {
            w.emit(
                json!({"type":"gap","reason":n["reason"],"count":1}),
                &json!({}),
            )?;
        }
        "session_state" => session(w, &n["row"])?,
        "message" => message(s, w, n, true)?,
        "event" => match d["type"].as_str().unwrap_or("") {
            "user" | "assistant" | "system" | "synthetic" => message(s, w, n, false)?,
            "agent-switched" | "model-switched" => {
                let mut r = json!({"type":"config_change"});
                if d["type"] == "agent-switched" {
                    copy(&mut r, "agent", d, "agent");
                } else if let Some(m) = model(&d["model"]) {
                    r["model"] = m;
                }
                preserve(&mut r, w, n);
                timestamp(&mut r, d, &n["row"]);
                if r.get("agent").is_some() || r.get("model").is_some() {
                    w.emit(r, &json!({}))?;
                } else {
                    w.vendor(n, "unmapped_config")?;
                }
            }
            "compaction" => {
                let mut r = json!({"type":"compaction","compactionId":n["row"]["id"],"kind":"summary","replaces":{"state":"unknown"}});
                if let Some(text) = d["summary"].as_str() {
                    r["summary"] = json!([{"type":"text","text":text}]);
                } else {
                    r["capture"] = json!({"summary":{"state":"absent"}});
                }
                copy(&mut r, "reason", d, "reason");
                if matches!(d["reason"].as_str(), Some("auto" | "manual")) {
                    r["trigger"] = d["reason"].clone();
                }
                preserve(&mut r, w, n);
                timestamp(&mut r, d, &n["row"]);
                w.emit(r, &json!({}))?;
            }
            "shell" => tools::shell(s, w, n)?,
            _ => {
                w.vendor(n, "native_event")?;
            }
        },
        "part" => {
            match d["type"].as_str().unwrap_or("") {
                "text" | "reasoning" | "file" => {
                    if d["ignored"] == true {
                        w.vendor(n, "ignored_part")?;
                    } else if content::part(d, w.provider).is_none() {
                        w.vendor(n, "unmapped_part")?;
                    } else {
                        // Folding several parts into a message must not discard
                        // their individual SQL rows or invent metadata destinations.
                        w.vendor(n, "part")?;
                    }
                } // folded into the owning message unless source excludes it
                "tool" => tools::tool(s, w, d, n, cwd, 0)?,
                "step-finish" => inference(w, d, &n["row"], n["row"]["id"].as_str().unwrap_or(""))?,
                "step-start" | "snapshot" => {
                    if let Some(snapshot) = d["snapshot"].as_str() {
                        let mut r = json!({"type":"checkpoint","kind":"snapshot","ref":snapshot});
                        preserve(&mut r, w, n);
                        w.emit(r, &json!({}))?;
                    } else {
                        w.vendor(n, "native_part")?;
                    }
                }
                "patch" => {
                    if let Some(files) = d["files"].as_array().filter(|a| !a.is_empty()) {
                        let changes: Vec<_> = files
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|p| {
                                let mut p = crate::records::path(p, cwd);
                                p["op"] = json!("_ai.opencode/changed");
                                p
                            })
                            .collect();
                        if !changes.is_empty() {
                            let mut r = json!({"type":"file_change","status":"applied","precision":"unknown","basis":"net","changes":changes});
                            preserve(&mut r, w, n);
                            w.emit(r, &json!({}))?;
                        }
                    } else {
                        w.vendor(n, "native_part")?;
                    }
                }
                "compaction" => {
                    let mut r = json!({"type":"compaction","compactionId":n["row"]["id"],"kind":"summary","replaces":{"state":"unknown"},"capture":{"summary":{"state":"absent"}}});
                    preserve(&mut r, w, n);
                    w.emit(r, &json!({}))?;
                }
                _ => {
                    w.vendor(n, "native_part")?;
                }
            }
        }
        "todo" => todo(s, w, n)?,
        other => {
            w.vendor(n, other)?;
        }
    }
    Ok(())
}

fn todo(s: &mut Spool, w: &mut Writer, n: &Value) -> Result<()> {
    // The native writer replaces the entire list. Individual rows are items,
    // not independent plans or historical revisions of a plan.
    if s.entity("todo_emitted")?.is_none() {
        s.save("todo_emitted", &json!(true))?;
        let mut entries = Vec::new();
        let mut bytes = 0;
        let mut observed = 0;
        let mut complete = true;
        loop {
            let next: Option<(u64, u64)> = s.db.query_row(
                "SELECT pos,observed FROM rows WHERE native_id LIKE 'todo:%' AND observed>?1 ORDER BY observed LIMIT 1",
                [observed], |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional()?;
            let Some((pos, order)) = next else { break };
            observed = order;
            let item = s.get(pos)?;
            let row = &item["row"];
            if let (Some(text), Some(status), Some(position)) = (
                row["content"].as_str(),
                row["status"].as_str(),
                row["position"].as_i64(),
            ) {
                let status = match status {
                    "pending" | "in_progress" | "completed" => status.to_owned(),
                    other => format!("_{}/{}", w.provider.domain(), other),
                };
                let mut entry =
                    json!({"entryId":position.to_string(),"content":text,"status":status});
                if let Some(priority) = row["priority"].as_str() {
                    entry["priority"] = json!(match priority {
                        "high" | "medium" | "low" => priority.to_owned(),
                        other => format!("_{}/{}", w.provider.domain(), other),
                    });
                }
                bytes += serde_json::to_vec(&entry)?.len();
                if complete && bytes <= s.limit {
                    entries.push(entry);
                } else {
                    complete = false;
                    entries.clear();
                }
            } else {
                complete = false;
                entries.clear();
            }
        }
        if complete {
            w.emit(json!({"type":"plan","planId":"todos","idSource":"derived","idScope":"capture","planType":"items","entries":entries}), &json!({}))?;
        } else {
            // ASR item plans require a complete inline entries array. Preserve
            // bounded native rows below when the aggregate cannot be projected.
            w.emit(
                json!({"type":"gap","reason":"conversion_loss","count":1}),
                &json!({}),
            )?;
        }
    }
    w.vendor(n, "todo")
}

fn message(s: &mut Spool, w: &mut Writer, n: &Value, split: bool) -> Result<()> {
    let d = &n["data"];
    let row = &n["row"];
    let Some(id) = row["id"].as_str() else {
        w.vendor(n, "unidentified_message")?;
        return Ok(());
    };
    let role = d["role"].as_str().or_else(|| d["type"].as_str());
    let mut r = json!({"type":"message","messageId":id,"content":[]});
    match role {
        Some("user" | "assistant" | "system" | "developer") => {
            r["role"] = json!(role);
        }
        Some("synthetic") => {
            r["role"] = json!("user");
            r["author"] = json!("harness");
            r["origin"] = json!("injected");
        }
        _ => {
            r["capture"]["role"] = json!({"state":"absent"});
        }
    }
    if role == Some("assistant") {
        r["author"] = json!("model");
    }
    if d.pointer("/time/completed").is_some_and(|v| !v.is_null()) {
        r["status"] = json!("complete");
    }
    if d.get("error").is_some_and(|v| !v.is_null()) {
        r["status"] = json!("error");
    }
    if split {
        content::fold(s, w, &mut r, id, d)?;
    } else {
        let mut parts = Vec::new();
        if let Some(t) = d["text"].as_str() {
            parts.push(json!({"type":"text","text":t}));
        }
        for p in d["content"].as_array().into_iter().flatten() {
            if let Some(p) = content::part(p, w.provider) {
                parts.push(p);
            }
        }
        for p in d["files"].as_array().into_iter().flatten() {
            if let Some(p) = content::part(p, w.provider) {
                parts.push(p);
            }
        }
        r["content"] = json!(parts);
        if d.get("text").is_none() && d.get("content").is_none() && d.get("files").is_none() {
            r["capture"]["content"] = json!({"state":"absent"});
        }
    }
    preserve(&mut r, w, n);
    timestamp(&mut r, d, row);
    let output = w.emit(r, &json!({}))?;
    s.set_output(id, &output)?;
    if !split {
        for (i, p) in d["content"].as_array().into_iter().flatten().enumerate() {
            if p["type"] == "tool" {
                tools::tool(s, w, p, n, None, 100 + (i as u64) * 10)?;
            }
        }
        if role == Some("assistant") && (d.get("tokens").is_some() || d.get("finish").is_some()) {
            w.slot(900_000);
            inference(w, d, row, id)?;
        }
    }
    Ok(())
}
pub(super) fn preserve(r: &mut Value, _w: &Writer, n: &Value) {
    r["native"] = json!({"record":n});
}
pub(super) fn timestamp(r: &mut Value, d: &Value, row: &Value) {
    if let Some(t) = d.pointer("/time/created").and_then(millis) {
        r["ts"] = json!(t);
    } else if let Some(t) = millis(&row["time_created"]) {
        r["ts"] = json!(t);
        r["tsSource"] = json!("container");
    }
}
fn model(m: &Value) -> Option<Value> {
    Some(
        json!({"id":m["modelID"].as_str().or_else(||m["id"].as_str())?,"provider":m["providerID"]}),
    )
}
fn usage(t: &Value) -> Value {
    let mut u = json!({});
    for (k, path) in [
        ("input", "/input"),
        ("read", "/cache/read"),
        ("write", "/cache/write"),
    ] {
        if let Some(n) = t.pointer(path).and_then(Value::as_u64) {
            u["input"][match k {
                "input" => "uncached",
                "read" => "cacheRead",
                _ => "cacheWrite",
            }] = json!(n);
        }
    }
    if let (Some(output), Some(reasoning)) = (t["output"].as_u64(), t["reasoning"].as_u64()) {
        if let Some(total) = output
            .checked_add(reasoning)
            .filter(|n| *n <= 9_007_199_254_740_991)
        {
            u["output"] = json!({"total":total,"reasoning":reasoning});
        }
    } else if let Some(reasoning) = t["reasoning"].as_u64() {
        u["output"]["reasoning"] = json!(reasoning);
    }
    u
}
fn inference(w: &mut Writer, d: &Value, row: &Value, id: &str) -> Result<()> {
    let mut r = json!({"type":"inference","inferenceId":format!("step:{id}"),"idSource":"derived"});
    let u = usage(&d["tokens"]);
    if u.as_object().is_some_and(|o| !o.is_empty()) {
        r["usage"] = u;
    }
    copy(
        &mut r,
        "stopReason",
        d,
        if d.get("finish").is_some() {
            "finish"
        } else {
            "reason"
        },
    );
    if let Some(cost) = d["cost"].as_f64() {
        r["cost"] = json!({"amount":cost,"currency":"USD","source":"harness_estimated"});
    }
    // The native model ref records request selection, not provider resolution.
    if let Some(m) = model(&d["model"]) {
        r["model"] = json!({"requested":m["id"]});
        if !m["provider"].is_null() {
            r["model"]["provider"] = m["provider"].clone();
        }
    }
    meta(&mut r, w.provider, "step", d);
    timestamp(&mut r, d, row);
    w.emit(r, &json!({}))?;
    Ok(())
}
fn session(w: &mut Writer, row: &Value) -> Result<()> {
    let mut r = json!({"type":"session_info"});
    copy(&mut r, "title", row, "title");
    if row.get("title").is_some_and(|v| !v.is_null()) {
        w.emit(r, &json!({}))?;
    }
    let mut tokens = json!({"cache":{}});
    for (a, b) in [
        ("input", "tokens_input"),
        ("output", "tokens_output"),
        ("reasoning", "tokens_reasoning"),
    ] {
        copy(&mut tokens, a, row, b);
    }
    copy(&mut tokens["cache"], "read", row, "tokens_cache_read");
    copy(&mut tokens["cache"], "write", row, "tokens_cache_write");
    let u = usage(&tokens);
    if !u.as_object().is_none_or(|o| o.is_empty()) || row["cost"].is_number() {
        w.slot(100);
        let mut r = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null});
        if u.as_object().is_some_and(|o| !o.is_empty()) {
            r["usage"] = u;
        }
        if row["cost"].is_number() {
            r["cost"] = json!({"amount":row["cost"],"currency":"USD","source":"harness_estimated"});
        }
        w.emit(r, &json!({}))?;
    }
    Ok(())
}

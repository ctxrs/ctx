//! DeepSeek Harness logical formats 0 and 4 (plain or concatenated zstd frames).
mod audit;
mod request;
pub(crate) mod source;
use crate::{
    content, jsonl,
    records::{copy, extras, meta, path, session_ref, Writer},
    spool::Spool,
    tools, Provider, Result,
};
use serde_json::{json, Value};
const P: Provider = Provider::DeepSeekHarness;
pub(crate) fn convert(
    spool: &mut Spool,
    w: &mut Writer,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let d = &v["data"];
    let kind = v["type"].as_str().unwrap_or("unknown");
    let time = jsonl::clock(P, v);
    if let Some(seed) = spool.entity("deepseek:seed")?.and_then(|v| v.as_u64()) {
        if v["seq"].as_u64().is_some_and(|seq| seq < seed) {
            if let Some(parent) = spool
                .entity("deepseek:parent")?
                .and_then(|v| v.as_str().map(str::to_owned))
            {
                w.copied =
                    Some(json!({"session":session_ref(P,&parent),"proof":"prefix_contract"}));
            }
        }
    }
    if let Some(turn) = d["turn"].as_i64() {
        w.turn = Some(turn.to_string());
    }
    if kind == "session" {
        spool.save("deepseek:header", v)?;
        return Ok(());
    }
    match kind {
        "_gap" => {
            w.emit(json!({"type":"gap","reason":v["reason"],"count":1}), &time)?;
        }
        "turn/start" | "turn/end" => {
            let mut r =
                json!({"type":"state","state":if kind=="turn/start"{"running"}else{"idle"}});
            if kind == "turn/end" {
                let reason = d["reason"]
                    .as_str()
                    .or_else(|| d["reason"]["kind"].as_str());
                if let Some(reason) = reason {
                    r["stopReason"] = json!(match reason {
                        "completed" => "end_turn",
                        "aborted" | "interrupted" => "cancelled",
                        "max-tokens" => "max_tokens",
                        other => other,
                    });
                }
                if let Some(error) = d["reason"].get("error") {
                    r["error"] = json!({});
                    copy(&mut r["error"], "message", error, "message");
                    copy(&mut r["error"], "code", error, "code");
                    meta(&mut r, P, "error", error);
                }
            }
            native(&mut r, v, &["turn", "reason"]);
            w.emit(r, &time)?;
        }
        "step/start" => {
            spool.save("deepseek:step", d)?;
            spool.save("deepseek:response", &Value::Null)?;
            spool.save("deepseek:usage", &Value::Null)?;
            w.vendor(v, "step_start")?;
        }
        "assistant/chunk" => request::chunk(spool, w, v, &time)?,
        "request/header" => request::header(spool, w, v, &time)?,
        "request/context" => {
            let mut r = json!({"type":"config_change","snapshot":true,"model":{"provider":d["provider"],"id":d["model"]}});
            native(&mut r, v, &["provider", "model"]);
            w.emit(r, &time)?;
        }
        "system/message" | "developer/message" | "user/message" | "assistant/message"
        | "tool/result" => surface(spool, w, v, &time, cwd)?,
        "tool/call" => request::call_started(spool, w, v, &time)?,
        "compaction/summary" | "compaction/prune" => {
            spool.save("deepseek:meter", v)?;
            if kind == "compaction/summary"
                && (d["llmStreamCall"] == true || d["usage"].is_object())
            {
                request::summary(spool, w, v, &time)?;
            } else {
                w.vendor(
                    v,
                    if kind == "compaction/summary" {
                        "compaction_summary"
                    } else {
                        "compaction_prune"
                    },
                )?;
            }
        }
        _ => {
            if !audit::convert(spool, w, v, &time, cwd)? {
                w.vendor(v, kind)?;
            }
        }
    }
    if let (Some(seq), Some(header)) = (v["seq"].as_u64(), spool.entity("deepseek:header")?) {
        if header["seedLength"]
            .as_u64()
            .or_else(|| {
                spool
                    .entity("deepseek:seed")
                    .ok()
                    .flatten()
                    .and_then(|v| v.as_u64())
            })
            .is_some_and(|n| seq < n)
        {
            if let Some(last) = w.last.as_ref() {
                spool.save("deepseek:copied_last", &json!(last))?;
            }
        }
    }
    if kind == "turn/end" {
        w.turn = None;
    }
    Ok(())
}
fn native(r: &mut Value, v: &Value, covered: &[&str]) {
    extras(r, P, &v["data"], covered);
    extras(r, P, v, &["data"]);
}
fn parts(v: &Value) -> Vec<Value> {
    if v.is_string() {
        return content::parts(v, P);
    }
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| match p["type"].as_str() {
            Some("tool-call" | "tool-result") => None,
            Some("reasoning") => {
                let mut r = json!({"type":"reasoning","text":p["text"]});
                extras(&mut r, P, p, &["text"]);
                Some(r)
            }
            Some("image") if p["attachment"].is_object() => {
                let mut r = json!({"type":"image","mimeType":p["attachment"]["mediaType"]});
                meta(&mut r, P, "attachment", &p["attachment"]);
                extras(&mut r, P, p, &["attachment"]);
                Some(r)
            }
            _ => content::parts(&json!([p]), P)
                .into_iter()
                .next()
                .map(|mut r| {
                    extras(
                        &mut r,
                        P,
                        p,
                        &["text", "data", "mimeType", "annotations", "_meta"],
                    );
                    r
                }),
        })
        .collect()
}
fn surface(
    spool: &mut Spool,
    w: &mut Writer,
    v: &Value,
    time: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let d = &v["data"];
    let kind = v["type"].as_str().unwrap_or("");
    let m = if matches!(
        kind,
        "system/message" | "developer/message" | "user/message"
    ) {
        d
    } else {
        &d["message"]
    };
    if v["surfaceOp"]["op"] == "replace" {
        return replacement(spool, w, v, m, time);
    }
    let Some(id) = m["id"].as_str() else {
        return w.vendor(v, "unidentified_message");
    };
    let mut r;
    if kind == "tool/result" {
        let block = if m["content"][0]["type"] == "tool-result" {
            &m["content"][0]
        } else {
            m
        };
        let call = m["source"]["callId"]
            .as_str()
            .or_else(|| block["toolCallId"].as_str());
        r = json!({"type":"tool_result","linkage":"none","status":jsonl::result_status(&block["isError"]),"content":parts(&block["content"])});
        if let Some(call) = call {
            r["callId"] = json!(call);
            r["linkage"] = json!(if spool.ambiguous_call(call)? {
                "ambiguous"
            } else {
                "exact"
            });
        }
        extras(&mut r, P, block, &["content", "toolCallId", "isError"]);
        if d["error"].is_object() {
            r["error"] = json!({"code":d["error"]["code"]});
            meta(&mut r, P, "error", &d["error"]);
        }
        binary_capture(&mut r, &block["content"]);
    } else {
        r = json!({"type":"message","messageId":id,"role":m["role"],"content":parts(&m["content"])});
        provenance(&mut r, &m["source"]);
        if kind == "assistant/message" {
            let inf = request::id(spool, id)?;
            r["inferenceId"] = json!(inf);
            spool.save(
                &format!("deepseek:assistant:{}:{}", d["turn"], d["step"]),
                &v["seq"],
            )?;
        }
        binary_capture(&mut r, &m["content"]);
    }
    extras(&mut r, P, m, &["content", "role", "id"]);
    native(
        &mut r,
        v,
        if kind == "user/message" {
            &["content", "role", "id", "source"]
        } else {
            &["message", "usage"]
        },
    );
    let record = w.emit(r, time)?;
    let mut nodes = vec![json!(record)];
    if kind == "assistant/message" {
        for block in m["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "tool-call")
        {
            if let Some(call) = request::call(spool, w, v, block, time, Some(id))? {
                nodes.push(json!(call));
            }
        }
    }
    if let Some(seq) = v["seq"].as_u64() {
        spool.save(&format!("deepseek:nodes:{seq}"), &json!(nodes))?;
        spool.set_output(&seq.to_string(), &record)?;
    }
    if kind == "assistant/message" {
        w.slot(800_000);
        request::inference(spool, w, v, time)?;
    }
    if kind == "tool/result" {
        audit::diffs(w, v, time, cwd)?;
    }
    Ok(())
}
fn binary_capture(r: &mut Value, content: &Value) {
    if content.as_array().is_some_and(|a| {
        a.iter()
            .any(|p| p["type"] == "image" && p["attachment"].is_object())
    }) {
        r["capture"] = json!({"content":{"state":"truncated","reason":"unsupported"}});
    }
}
fn provenance(r: &mut Value, s: &Value) {
    match s["kind"].as_str() {
        Some("user") => {
            r["author"] = json!("human");
            r["origin"] = json!("prompt");
        }
        Some("model") => {
            r["author"] = json!("model");
        }
        Some("plugin" | "goal" | "subagent-settled") => {
            r["author"] = json!("harness");
            r["origin"] = json!("injected");
        }
        Some("coordinator" | "subagent-report") => {
            r["author"] = json!("agent");
            r["origin"] = json!(if s["kind"] == "subagent-report" {
                "subagent_report"
            } else {
                "inter_session"
            });
            if let Some(id) = s["senderSessionId"].as_str() {
                r["from"] = json!({"session":session_ref(P,id)});
            }
        }
        Some("tool") => {
            r["author"] = json!("tool");
        }
        _ => {}
    }
    if s["form"] == "recall" {
        r["origin"] = json!("retrieval");
    }
    meta(r, P, "messageSource", s);
}
fn replacement(spool: &Spool, w: &mut Writer, v: &Value, m: &Value, time: &Value) -> Result<()> {
    let meter = spool.entity("deepseek:meter")?.filter(|p| {
        p["seq"]
            .as_u64()
            .zip(v["seq"].as_u64())
            .is_some_and(|(a, b)| a + 1 == b)
    });
    let d = meter.as_ref().map(|m| &m["data"]);
    let mut ids = Vec::new();
    let mut missing = false;
    if let Some(seqs) = d.and_then(|d| d["shadowedSeqs"].as_array()) {
        for seq in seqs {
            if let Some(nodes) = seq
                .as_u64()
                .map(|s| spool.entity(&format!("deepseek:nodes:{s}")))
                .transpose()?
                .flatten()
            {
                ids.extend(nodes.as_array().into_iter().flatten().cloned());
            } else {
                missing = true;
            }
        }
    } else {
        missing = true;
    }
    let id = d
        .and_then(|d| d["compactionId"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("replace:{}", v["seq"]));
    let mut r = json!({"type":"compaction","compactionId":id,"kind":if meter.as_ref().is_some_and(|m|m["type"]=="compaction/prune"){"mask"}else{"summary"},"status":"completed","summary":if v["type"]=="tool/result" && m["content"][0]["type"]=="tool-result"{parts(&m["content"][0]["content"])}else{parts(&m["content"])},"historyRetained":true,"replaces":if missing{json!({"state":"unknown"})}else{json!({"ids":ids})}});
    if d.and_then(|d| d["compactionId"].as_str()).is_none() {
        r["idSource"] = json!("derived");
    }
    if !missing {
        if let Some(first) = ids.first() {
            r["insertAt"] = json!({"beforeId":first});
        }
    }
    native(&mut r, v, &[]);
    if let Some(meter) = &meter {
        meta(&mut r, P, "replacementMeter", meter);
    }
    let record = w.emit(r, time)?;
    if let Some(seq) = v["seq"].as_u64() {
        spool.save(&format!("deepseek:nodes:{seq}"), &json!([record]))?;
        spool.set_output(&seq.to_string(), &record)?;
    }
    Ok(())
}

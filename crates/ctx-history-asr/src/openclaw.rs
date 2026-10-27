//! OpenClaw v17/v19 JSON and v37 JSON/zstd storage, read independently of Core.
mod messages;
mod side;
mod source;
mod structure;
mod tools;
use crate::{
    native as n,
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
pub(crate) use source::{ingest, visit};
pub(super) const P: Provider = Provider::OpenClaw;

pub(crate) fn convert(s: &mut Spool, w: &mut Writer, h: &mut Value, v: &Value) -> Result<()> {
    if v["table"] != "transcript_events" {
        return side::convert(s, w, h, v);
    }
    let e = &v["row"];
    let kind = e["type"].as_str().unwrap_or("unknown");
    if kind == "session" {
        return Ok(());
    }
    let id = e["id"].as_str();
    let Some(id) = id else {
        n::emit(
            s,
            w,
            json!({"type":"gap","reason":"unparseable","count":1}),
            v,
        )?;
        return Ok(());
    };
    let root = if let Some(previous) = e["supersedesEntryId"].as_str() {
        s.entity(&format!("root:{previous}"))?
            .unwrap_or(json!(previous))
    } else {
        json!(id)
    };
    s.save(&format!("root:{id}"), &root)?;
    w.turn = run(e).map(str::to_owned).or(s
        .entity(&format!("input-run:{id}"))?
        .and_then(|v| v.as_str().map(str::to_owned)));
    let mut records = match kind {
        "message" => messages::convert(s, h, e, root.as_str().unwrap_or(id))?,
        "custom_message" => {
            let mut x = e.clone();
            x["message"] = e.clone();
            x["message"]["role"] = json!("custom");
            messages::convert(s, h, &x, root.as_str().unwrap_or(id))?
        }
        "model_change" => {
            let mut r = json!({"type":"config_change","model":{"id":e["modelId"]}});
            copy(&mut r["model"], "provider", e, "provider");
            vec![r]
        }
        "thinking_level_change" => {
            vec![json!({"type":"config_change","effort":e["thinkingLevel"]})]
        }
        "compaction" | "reset" | "branch_summary" => vec![structure::compaction(s, e)?],
        "leaf" => match structure::attachment(s, &e["targetId"])? {
            Some(to) => vec![
                json!({"type":"rewind","to":to,"restoreRequested":false,"reason":if s.entity("window")?.is_some_and(|w|w["reason"]=="rewind"){"user_rewind"}else{"other"}}),
            ],
            None => {
                n::loss(s, "branches")?;
                vec![json!({"type":"gap","reason":"not_retained"})]
            }
        },
        "session_info" => vec![json!({"type":"session_info","title":e["name"]})],
        "label" => {
            vec![json!({"type":"ai.openclaw/label","targetId":e["targetId"],"label":e["label"]})]
        }
        "custom" if e["customType"] == "openclaw.system-prompt" => {
            let mut r = json!({"type":"instructions","kind":"system_prompt"});
            if e["data"]["renderedPrefix"].is_string() {
                r["content"] = json!(messages::parts(&e["data"]["renderedPrefix"]));
            } else {
                r["capture"] = json!({"content":{"state":"absent"}});
            }
            n::retain(&mut r, P, "prompt", &e["data"]);
            vec![r]
        }
        _ => {
            n::loss(s, "_ai.openclaw/vendor_only")?;
            vec![json!({"type":"ai.openclaw/custom","_meta":{"ai.openclaw/entry":e}})]
        }
    };
    let response = e["message"]["responseId"]
        .as_str()
        .or(e["message"]["turnId"].as_str())
        .map(str::to_owned)
        .or(s
            .entity(&format!(
                "call:{}",
                e["message"]["toolCallId"].as_str().unwrap_or("")
            ))?
            .and_then(|v| v["inferenceId"].as_str().map(str::to_owned)));
    let mut first = true;
    let mut outputs = Vec::new();
    for mut r in records.drain(..) {
        if r.get("ts").is_none() {
            n::set_time(&mut r, "ts", &e["timestamp"]);
        }
        if let Some(run) = w.turn.as_deref() {
            if r["type"] != "compaction" && s.entity(&format!("seen-turn:{run}"))?.is_none() {
                s.save(&format!("seen-turn:{run}"), &json!(true))?;
                if let Some(t) = s.entity(&format!("trigger:{run}"))? {
                    r["trigger"] = t;
                }
            }
        }
        if let Some(a) = e.get("appendMode") {
            meta(&mut r, P, "appendMode", a);
        }
        if v["copiedFrom"].is_string() {
            r["copiedFrom"] = json!({"session":v["copiedFrom"],"proof":"native_id"});
        }
        structure::rewrite(s, e, &mut r)?;
        if crate::records::context(&r) {
            structure::place(s, w, e, &mut r, response.as_deref(), first)?;
            first = false;
        }
        let out = n::emit(s, w, r.clone(), v)?;
        if crate::records::context(&r) {
            structure::remember(s, w, e, &r, &out, response.as_deref())?;
        }
        r["_record"] = json!(out);
        if r["type"] == "message" {
            s.save(&format!("message:{id}"), &r)?;
        }
        if r["type"] == "inference" {
            s.save(
                &format!("inference:{}", r["inferenceId"].as_str().unwrap_or("")),
                &r,
            )?;
        }
        outputs.push(r);
    }
    s.save(&format!("unit:{id}"), &json!(outputs))?;
    if v["copiedPrefix"] == true {
        if let Some(last) = w.last.as_ref() {
            h["lineage"]["inherits"]["localThroughId"] = json!(last);
        }
    }
    Ok(())
}
pub(super) fn run(e: &Value) -> Option<&str> {
    e.pointer("/message/__openclaw/runId")
        .and_then(Value::as_str)
        .or(e.pointer("/__openclaw/runId").and_then(Value::as_str))
        .or(e
            .pointer("/message/__openclaw/steerTargetRunId")
            .and_then(Value::as_str))
        .or_else(|| {
            (e["message"]["customType"] == "openclaw.nested-tool.v1")
                .then(|| e.pointer("/message/details/runId").and_then(Value::as_str))
                .flatten()
        })
}

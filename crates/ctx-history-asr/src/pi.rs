//! Pi JSONL v1-v3. The session header, never its filename, owns identity.
pub(crate) mod copies;
mod parts;
use crate::{
    content, jsonl,
    records::{copy, extras, meta, path, Writer},
    spool::Spool,
    tools, Provider, Result,
};
use serde_json::{json, Value};
const P: Provider = Provider::Pi;

pub(crate) fn convert(
    spool: &mut Spool,
    w: &mut Writer,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let kind = v["type"].as_str().unwrap_or("unknown");
    let id = v["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("entry:{}", w.pos));
    match kind {
        "session" => {}
        "_gap" => {
            w.emit(json!({"type":"gap","reason":v["reason"],"count":1}), v)?;
        }
        "message" => message(spool, w, v, &id, cwd)?,
        "custom_message" => {
            let mut r = json!({"type":"message","messageId":id,"role":"user","author":"harness","origin":"injected","content":parts::parts(&v["content"])});
            if let Some(display) = v["display"].as_bool() {
                r["audience"] = json!(if display { "both" } else { "model" });
            }
            extras(&mut r, P, v, &["id", "parentId", "content"]);
            emit(spool, w, r, v, None, true)?;
        }
        "model_change" if v["modelId"].is_string() => {
            let mut r = json!({"type":"config_change","model":{"id":v["modelId"]}});
            copy(&mut r["model"], "provider", v, "provider");
            extras(&mut r, P, v, &["modelId", "provider"]);
            w.emit(r, v)?;
        }
        "thinking_level_change" if v["thinkingLevel"].is_string() => {
            let mut r = json!({"type":"config_change","effort":v["thinkingLevel"]});
            extras(&mut r, P, v, &["thinkingLevel"]);
            w.emit(r, v)?;
        }
        "title" if v["v"] == 1 && v["title"].is_string() => {
            spool.save("pi:title", v)?;
        }
        "session_info" if v.get("name").is_some() => {
            let mut r = json!({"type":"session_info","title":v["name"],"titleSource":"user"});
            extras(&mut r, P, v, &["name"]);
            w.emit(r, v)?;
        }
        "compaction" | "branch_summary" => compaction(spool, w, v, &id)?,
        _ => w.vendor(v, kind)?,
    }
    Ok(())
}
pub(crate) fn finish(spool: &Spool, w: &mut Writer) -> Result<()> {
    if let Some(title) = spool.entity("pi:title")? {
        // OMP rewrites this slot in place. Current state follows all source-positioned records.
        w.position(w.pos + 1);
        let mut r = json!({"type":"session_info","title":title["title"],"assembly":{"mode":"snapshot"},"idSource":"derived","idScope":"capture"});
        extras(&mut r, P, &title, &["title"]);
        w.emit(r, &json!({"timestamp":title["updatedAt"]}))?;
    }
    Ok(())
}
fn emit(
    spool: &Spool,
    w: &mut Writer,
    mut r: Value,
    v: &Value,
    response: Option<&str>,
    first: bool,
) -> Result<()> {
    if v["id"].as_str().is_none() {
        r["idSource"] = json!("derived");
        r["idScope"] = json!("capture");
    }
    // Native units may split into text and calls. Only the first receives a branch edge.
    crate::tree::emit(spool, w, r, v, response, first, ("id", "parentId"))
}
fn message(
    spool: &mut Spool,
    w: &mut Writer,
    v: &Value,
    id: &str,
    cwd: Option<&str>,
) -> Result<()> {
    let m = &v["message"];
    let role = m["role"].as_str().unwrap_or("unknown");
    if role == "bashExecution" {
        return bash(spool, w, v, id, cwd);
    }
    if role == "toolResult" {
        let Some(call) = m["toolCallId"].as_str() else {
            return w.vendor(v, "message");
        };
        let mut r = json!({"type":"tool_result","callId":call,"linkage":if spool.ambiguous_call(call)?{"ambiguous"}else{"exact"},"status":jsonl::result_status(&m["isError"]),"content":parts::parts(&m["content"])});
        copy(&mut r, "raw", m, "content");
        extras(&mut r, P, m, &["role", "toolCallId", "isError", "content"]);
        extras(&mut r, P, v, &["message"]);
        if m.get("content").is_none() {
            r["capture"] = json!({"content":{"state":"absent"}});
        }
        if m.pointer("/details/truncation/truncated") == Some(&json!(true)) {
            r["capture"] = json!({"raw":{"state":"truncated","reason":"source_truncated"}});
        }
        let call_data = if spool.ambiguous_call(call)? {
            None
        } else {
            spool.entity(&format!("call:{call}"))?
        };
        let response = call_data.as_ref().and_then(|c| c["source"].as_str());
        emit(spool, w, r, v, response, true)?;
        if let Some(call_data) = call_data {
            file_change(w, v, call, &call_data, m, cwd)?;
        }
        return Ok(());
    }
    if !matches!(role, "assistant" | "user" | "system" | "custom") {
        return w.vendor(v, "message");
    }
    let inference =
        if role == "assistant" && (m["responseId"].is_string() || m["usage"].is_object()) {
            Some(
                m["responseId"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("response:{id}")),
            )
        } else {
            None
        };
    let response = m["responseId"].as_str().or(inference.as_deref());
    let mut r = json!({"type":"message","messageId":id,"role":if role=="custom"{"user"}else{role},"content":parts::parts(&m["content"])});
    if role == "assistant" {
        r["author"] = json!("model");
    }
    if role == "custom" {
        r["author"] = json!("harness");
        r["origin"] = json!("injected");
    }
    if m["stopReason"] == "error" {
        r["status"] = json!("error");
    }
    if m["stopReason"] == "aborted" {
        r["status"] = json!("interrupted");
    }
    if m["stopReason"] == "pending" {
        r["status"] = json!("partial");
    }
    if let Some(inf) = &inference {
        r["inferenceId"] = json!(inf);
    }
    extras(
        &mut r,
        P,
        m,
        &[
            "role",
            "content",
            "usage",
            "provider",
            "model",
            "responseModel",
            "responseId",
            "stopReason",
        ],
    );
    if inference.is_none() {
        for field in ["model", "provider", "responseModel"] {
            if let Some(value) = m.get(field) {
                meta(&mut r, P, field, value);
            }
        }
    }
    extras(&mut r, P, v, &["message", "id", "parentId"]);
    if m.get("content").is_none() {
        r["capture"] = json!({"content":{"state":"absent"}});
    }
    // Keep one native message even for an all-tool response. Its calls follow as one unit.
    let first = w.id();
    emit(spool, w, r, v, response, true)?;
    spool.save(&format!("pi:first:{id}"), &json!(first))?;
    for block in m["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["type"] == "toolCall")
    {
        let Some(call) = block["id"].as_str() else {
            w.vendor(block, "unidentified_call")?;
            continue;
        };
        let mut r = tools::call(
            w,
            spool,
            v,
            call,
            block["name"].as_str(),
            tools::Arguments::Json(block["arguments"].clone()),
            "model",
        )?;
        if let Some(inf) = &inference {
            r["inferenceId"] = json!(inf);
        }
        extras(&mut r, P, block, &["id", "name", "arguments"]);
        if let Some(signature) = block.get("thoughtSignature") {
            meta(&mut r, P, "thoughtSignature", signature);
        }
        spool.save(
            &format!("call:{call}"),
            &json!({"name":block["name"],"input":block["arguments"],"source":response}),
        )?;
        emit(spool, w, r, v, response, false)?;
    }
    if let Some(inf) = inference {
        w.slot(800_000);
        let mut r = json!({"type":"inference","inferenceId":inf,"purpose":"turn"});
        if !m["responseId"].is_string() {
            r["idSource"] = json!("derived");
        }
        let mut model = json!({});
        copy(&mut model, "provider", m, "provider");
        copy(&mut model, "requested", m, "model");
        if m["responseModel"].is_string() {
            copy(&mut model, "id", m, "responseModel");
        } else {
            copy(&mut model, "id", m, "model");
        }
        if model.as_object().is_some_and(|o| !o.is_empty()) {
            r["model"] = model;
        }
        let usage = jsonl::usage(&m["usage"], true);
        let placeholder = matches!(m["stopReason"].as_str(), Some("error" | "aborted"))
            && ["input", "output", "cacheRead", "cacheWrite", "totalTokens"]
                .iter()
                .all(|k| m["usage"][*k].as_u64().unwrap_or(0) == 0);
        if !placeholder && usage.as_object().is_some_and(|o| !o.is_empty()) {
            r["usage"] = usage;
        }
        if let Some(n) = m
            .pointer("/usage/cacheWrite1h")
            .filter(|_| !placeholder)
            .and_then(Value::as_u64)
        {
            r["usage"]["input"]["cacheWriteByTtl"] = json!({"1h":n});
        }
        if let Some(cost) = m
            .pointer("/usage/cost/total")
            .filter(|_| !placeholder)
            .and_then(Value::as_f64)
        {
            r["cost"] = json!({"amount":cost,"currency":"USD","source":"harness_estimated"});
        }
        if let Some(cost) = m.pointer("/usage/cost") {
            meta(&mut r, P, "costComponents", cost);
        }
        copy(&mut r, "stopReason", m, "stopReason");
        if let Some(usage) = m.get("usage") {
            meta(&mut r, P, "usage", usage);
        }
        if let Some(error) = m["errorMessage"].as_str() {
            r["error"] = json!({"message":error});
        }
        w.emit(r, v)?;
    }
    Ok(())
}
fn compaction(spool: &Spool, w: &mut Writer, v: &Value, id: &str) -> Result<()> {
    let branch = v["type"] == "branch_summary";
    let mut r = json!({"type":"compaction","compactionId":id,"kind":if branch{"branch_summary"}else{"summary"},"status":"completed","summary":content::parts(&v["summary"],P),"replaces":{"state":"unknown"},"historyRetained":true});
    // fromId is the abandoned leaf, not the full summarized extent.
    if !branch {
        if let Some(kept) = v["firstKeptEntryId"].as_str() {
            if let Some(first) = spool.entity(&format!("pi:first:{kept}"))? {
                let tip = if v.get("parentId").is_some() {
                    crate::tree::native_parent(spool, v, "parentId")?
                } else {
                    w.tip.clone()
                };
                if let Some(tip) = tip {
                    r["replaces"] = json!({"throughId":tip});
                    r["keptFromId"] = first;
                }
            }
        }
    }
    if let Some(n) = v["tokensBefore"].as_u64() {
        r["tokens"] = json!({"before":n});
    }
    extras(&mut r, P, v, &["summary", "tokensBefore", "usage"]);
    emit(spool, w, r, v, None, true)?;
    if v["usage"].is_object() {
        let mut usage = json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":null,"usage":jsonl::usage(&v["usage"],true)});
        meta(&mut usage, P, "compactionUsage", &v["usage"]);
        w.emit(usage, v)?;
    }
    Ok(())
}
fn bash(spool: &Spool, w: &mut Writer, v: &Value, id: &str, cwd: Option<&str>) -> Result<()> {
    let m = &v["message"];
    let mut terminal =
        json!({"type":"terminal","terminalId":format!("bash:{id}"),"idSource":"derived"});
    copy(&mut terminal, "command", m, "command");
    copy(&mut terminal, "exitCode", m, "exitCode");
    if m["cancelled"] == true {
        terminal["status"] = json!("stopped");
    } else if m["exitCode"].is_number() {
        terminal["status"] = json!("exited");
    }
    if let Some(out) = m["output"].as_str() {
        terminal["output"] = json!({"text":out});
    }
    if let Some(cwd) = cwd {
        terminal["cwd"] = path(cwd, Some(cwd));
    }
    if m["truncated"] == true {
        terminal["capture"] = json!({"output":{"state":"truncated","reason":"source_truncated"}});
    }
    extras(
        &mut terminal,
        P,
        m,
        &["command", "output", "exitCode", "role"],
    );
    w.emit(terminal, v)?;
    // The source's convertToLlm/bashExecutionToText projection is a harness-injected user message.
    let mut r = json!({"type":"message","messageId":id,"role":"user","author":"harness","origin":"injected","content":[]});
    if m["excludeFromContext"] == true {
        r["audience"] = json!("user");
        r["capture"] = json!({"content":{"state":"absent"}});
    } else if let Some(command) = m["command"].as_str() {
        let mut text = format!("Ran `{command}`\n");
        if let Some(out) = m["output"].as_str().filter(|s| !s.is_empty()) {
            text.push_str(&format!("```\n{out}\n```"));
        } else {
            text.push_str("(no output)");
        }
        if m["cancelled"] == true {
            text.push_str("\n\n(command cancelled)");
        } else if let Some(code) = m["exitCode"].as_i64().filter(|n| *n != 0) {
            text.push_str(&format!("\n\nCommand exited with code {code}"));
        }
        if m["truncated"] == true {
            if let Some(path) = m["fullOutputPath"].as_str() {
                text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
            }
        }
        r["content"] = json!([{"type":"text","text":text}]);
        r["audience"] = json!("model");
    } else {
        r["capture"] = json!({"content":{"state":"absent"}});
    }
    extras(&mut r, P, v, &["message"]);
    emit(spool, w, r, v, None, true)
}
fn file_change(
    w: &mut Writer,
    v: &Value,
    call: &str,
    request: &Value,
    m: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let name = request["name"].as_str().unwrap_or("");
    if !matches!(name, "edit" | "write") {
        return Ok(());
    }
    let Some(file) = request["input"]["path"].as_str() else {
        return Ok(());
    };
    let status = match m["isError"].as_bool() {
        Some(false) => "applied",
        Some(true) => "failed",
        None => "unknown",
    };
    let mut change = path(file, cwd);
    change["op"] = json!(if name == "edit" {
        "modify"
    } else {
        "_io.github.badlogic.pi/write"
    });
    let mut r = json!({"type":"file_change","cause":{"callId":call},"status":status,"precision":"unknown","changes":[change]});
    if let Some(diff) = m.pointer("/details/diff").and_then(Value::as_str) {
        meta(&mut r, P, "diff", &json!(diff));
    }
    if name == "edit" {
        r["precision"] = json!("snippet");
    }
    meta(&mut r, P, "request", &request["input"]);
    // write overwrites or creates; the request alone cannot distinguish add from modify.
    w.emit(r, v)?;
    Ok(())
}

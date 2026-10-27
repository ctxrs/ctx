use super::{tree, values};
use crate::{
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};

pub(super) fn calls(provider: Provider, message: &Value) -> Vec<Value> {
    message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|call| {
            let function = call.get("function").unwrap_or(call);
            let mut out = json!({"native":call});
            copy(
                &mut out,
                "id",
                call,
                if provider == Provider::ForgeCode {
                    "call_id"
                } else {
                    "id"
                },
            );
            copy(&mut out, "name", function, "name");
            if let Some(input) = function
                .get("arguments")
                .or_else(|| call.get("args"))
                .or_else(|| call.get("input"))
            {
                out["input"] = input.clone();
            }
            out
        })
        .collect()
}

pub(super) fn call_counts(v: &Value) -> std::collections::HashMap<&str, usize> {
    let mut counts = std::collections::HashMap::new();
    for call in v["calls"].as_array().into_iter().flatten() {
        if let Some(id) = call["id"].as_str() {
            *counts.entry(id).or_insert(0) += 1;
        }
    }
    counts
}

pub(super) fn convert(spool: &mut Spool, writer: &mut Writer, v: &Value) -> Result<()> {
    let m = &v["body"];
    let counts = call_counts(v);
    writer.slot(0);
    let role = v["role"].as_str();
    let mut first = true;
    let mut source = json!({});
    if let Some(ts) = v["timestamp"].as_str().and_then(crate::records::timestamp) {
        source["timestamp"] = json!(ts);
    }
    if role == Some("tool") {
        let mut r = json!({"type":"tool_result","status":"unknown","linkage":"none"});
        result_link(spool, &mut r, v["callId"].as_str())?;
        if let Some(status) = v["status"].as_str() {
            r["status"] = json!(status);
        }
        values::content(spool, &mut r, v.get("content"), writer.provider)?;
        if let Some(raw) = v.get("raw") {
            if values::safe(raw) {
                r["raw"] = raw.clone();
            } else {
                r["capture"]["raw"] = json!({"state":"external","blob":values::bytes(spool,&ctx_history_asr_core::numbers::text(raw).into_bytes(),"application/json")?});
            }
        }
        if let Some(exec) = v.get("exec").filter(|v| values::safe(v)) {
            r["exec"] = exec.clone();
        }
        if let Some(duration) = v["durationMs"].as_f64().filter(|n| *n >= 0.0) {
            r["durationMs"] = json!(duration);
        }
        if let Some(time) = v["completedAt"]
            .as_str()
            .and_then(crate::records::timestamp)
        {
            r["completedAt"] = json!(time);
        }
        emit(spool, writer, r, v, &source, &mut first)?;
    } else if v.get("content").is_some()
        || v["calls"].as_array().is_none_or(Vec::is_empty)
        || v.get("reasoning").is_some()
    {
        let native_id = v["messageId"].as_str();
        let mut r = json!({"type":"message","messageId":native_id.map(str::to_owned).unwrap_or_else(|| format!("{}-message",writer.id()))});
        if native_id.is_none() {
            r["idSource"] = json!("derived");
        }
        if let Some(role @ ("user" | "assistant" | "system" | "developer")) = role {
            r["role"] = json!(role);
        } else {
            r["capture"]["role"] = json!({"state":"absent"});
        }
        values::content(spool, &mut r, v.get("content"), writer.provider)?;
        if let Some(reasoning) = v["reasoning"].as_array() {
            if !reasoning.is_empty() {
                let mut parts = reasoning.clone();
                parts.extend(r["content"].as_array().into_iter().flatten().cloned());
                r["content"] = json!(parts);
                if let Some(c) = r.get_mut("capture").and_then(Value::as_object_mut) {
                    c.remove("content");
                }
            }
        }
        for key in ["author", "origin", "phase", "status"] {
            copy(&mut r, key, v, key);
        }
        emit(spool, writer, r, v, &source, &mut first)?;
    }
    for (index, call) in v["calls"].as_array().into_iter().flatten().enumerate() {
        // Content arriving later must not renumber a native call. A unique
        // call ID also survives insertion/reordering within its native unit.
        writer.slot(index as u64 + 1);
        writer.exact_id =
            if let Some(id) = call["id"].as_str().filter(|id| counts.get(id) == Some(&1)) {
                use sha2::{Digest, Sha256};
                Some(format!(
                    "r{:x}",
                    Sha256::digest(json!([v["key"], "call", id]).to_string().as_bytes())
                ))
            } else {
                None
            };
        writer.capture_scope = v["captureScoped"] == true || writer.exact_id.is_none();
        let mut r = json!({"type":"tool_call","callId":call["id"].as_str().map(str::to_owned).unwrap_or_else(|| format!("{}-call",writer.id())),"callIdSource":if call["id"].is_string(){"native"}else{"derived"}});
        if let Some(name) = call["name"].as_str() {
            r["name"] = json!(name);
        } else {
            r["capture"]["name"] = json!({"state":"absent"});
        }
        if role == Some("assistant") {
            r["requestedBy"] = json!("model");
        }
        if let Some(input) = call.get("input") {
            if let Some(text) = input.as_str() {
                if let Some(parsed) = super::sqlite::exact_json(text).filter(values::safe) {
                    r["input"] = parsed;
                } else {
                    r["inputText"] = json!(text);
                }
            } else if values::safe(input) {
                r["input"] = input.clone();
            } else {
                r["inputText"] = json!(ctx_history_asr_core::numbers::text(input));
            }
        } else {
            r["capture"]["input"] = json!({"state":"absent"});
        }
        if writer.provider == Provider::Devin && r["name"] == "mcp_call_tool" {
            if let (Some(server), Some(tool)) = (
                r["input"]["server_name"].as_str(),
                r["input"]["tool_name"].as_str(),
            ) {
                r["provider"] =
                    json!({"kind":"mcp","identity":"exact","server":server,"tool":tool});
            }
        }
        values::preserve(spool, &mut r, writer.provider, "call", &call["native"])?;
        emit(spool, writer, r, v, &source, &mut first)?;
    }
    writer.exact_id = None;
    writer.capture_scope = v["captureScoped"] == true;
    writer.slot(crate::records::STRIDE - 1);
    inference(spool, writer, v, m)?;
    Ok(())
}
pub(super) fn result_link(spool: &Spool, r: &mut Value, call: Option<&str>) -> Result<()> {
    if let Some(call) = call {
        if spool.ambiguous_call(call)? {
            r["linkage"] = json!("ambiguous");
        } else {
            r["callId"] = json!(call);
            r["linkage"] = json!("exact");
        }
    }
    Ok(())
}
pub(super) fn emit(
    spool: &Spool,
    writer: &mut Writer,
    mut r: Value,
    v: &Value,
    source: &Value,
    first: &mut bool,
) -> Result<()> {
    if matches!(r["type"].as_str(), Some("message" | "tool_call")) && v["role"] == "assistant" {
        if let Some(id) = inference_id(spool, writer.provider, v)? {
            r["inferenceId"] = json!(id);
        }
    }
    if *first {
        values::preserve(spool, &mut r, writer.provider, "source", &v["native"])?;
    }
    if v["tsSource"] == "container" {
        r["tsSource"] = json!("container");
    }
    let mut deep_key = if writer.provider == Provider::DeepAgents {
        v["messageId"].as_str().map(|id| {
            format!(
                "batch8:deepTarget:{}",
                json!([id, r["type"], r.get("callId")])
            )
        })
    } else {
        None
    };
    if r["type"] == "tool_call"
        && spool
            .entity(&format!(
                "batch8:duplicateInMessage:{}",
                json!([v["messageId"], r["callId"]])
            ))?
            .is_some()
    {
        if let Some(key) = &mut deep_key {
            key.push_str(&format!(":{}", writer.id()));
        }
    }
    if let Some(key) = &deep_key {
        if let Some(previous) = spool.entity(key)?.filter(|p| p["active"] == true) {
            r["supersedes"] = previous["record"].clone();
        }
    }
    let mut retained = r.clone();
    let result_call = (r["type"] == "tool_result")
        .then(|| r["callId"].as_str().map(str::to_owned))
        .flatten();
    let expected = writer.id();
    if v.get("node").is_some() {
        tree::emit(spool, writer, r, v, source, *first)?;
    } else {
        writer.emit(r, source)?;
    }
    if let Some(call) = result_call.filter(|_| v.get("node").is_none()) {
        retained["_record"] = json!(expected);
        spool.save(&format!("batch8:result:{call}"), &retained)?;
    }
    if let Some(key) = deep_key {
        spool.save(
            &key,
            &json!({"message":v["messageId"],"record":expected,"active":true}),
        )?;
    }
    *first = false;
    Ok(())
}

fn inference_id(spool: &Spool, provider: Provider, v: &Value) -> Result<Option<String>> {
    if provider == Provider::Devin {
        if let Some(request) = v["inferenceId"].as_str() {
            let count: u64 = spool.db.query_row(
                "SELECT count(*) FROM rows WHERE response=?1",
                [request],
                |r| r.get(0),
            )?;
            return Ok((count == 1).then(|| request.to_owned()));
        }
        return Ok(None);
    }
    if v.get("usage").is_none_or(Value::is_null) {
        return Ok(None);
    }
    if provider == Provider::DeepAgents {
        if let Some(message) = v["messageId"].as_str() {
            return Ok(Some(format!("message:{message}")));
        }
    }
    use sha2::{Digest, Sha256};
    Ok(Some(format!(
        "inference:{:x}",
        Sha256::digest(v["key"].to_string().as_bytes())
    )))
}
fn inference(spool: &Spool, writer: &mut Writer, v: &Value, _m: &Value) -> Result<()> {
    let Some(id) = inference_id(spool, writer.provider, v)? else {
        return Ok(());
    };
    let u = &v["usage"];
    let mut r = json!({"type":"inference","inferenceId":id});
    if !v["inferenceId"].is_string() {
        r["idSource"] = json!("derived");
    }
    let mut usage = json!({});
    let fields: &[(&str, &str)] = match writer.provider {
        Provider::ForgeCode => &[
            ("prompt_tokens", "/input/total"),
            ("completion_tokens", "/output/total"),
            ("total_tokens", "/total"),
            ("cached_tokens", "/input/cacheRead"),
        ],
        Provider::DeepAgents => &[
            ("input_tokens", "/input/total"),
            ("output_tokens", "/output/total"),
            ("total_tokens", "/total"),
        ],
        Provider::Devin => &[
            ("input_tokens", "/input/total"),
            ("output_tokens", "/output/total"),
            ("cache_read_tokens", "/input/cacheRead"),
            ("cache_creation_tokens", "/input/cacheWrite"),
        ],
        _ => &[],
    };
    for (key, path) in fields {
        let count = if writer.provider == Provider::ForgeCode {
            &u[*key]["Actual"]
        } else {
            &u[*key]
        };
        if let Some(n) = count.as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
            let names: Vec<_> = path.trim_start_matches('/').split('/').collect();
            if names.len() == 2 {
                usage[names[0]][names[1]] = json!(n);
            } else {
                usage[names[0]] = json!(n);
            }
        }
    }
    if writer.provider == Provider::DeepAgents {
        for (field, group, key) in [
            ("/input_token_details/cache_read", "input", "cacheRead"),
            ("/input_token_details/cache_creation", "input", "cacheWrite"),
            ("/output_token_details/reasoning", "output", "reasoning"),
        ] {
            if let Some(n) = u
                .pointer(field)
                .and_then(Value::as_u64)
                .filter(|n| *n <= 9_007_199_254_740_991)
            {
                usage[group][key] = json!(n);
            }
        }
    }
    if writer.provider == Provider::Devin {
        for (from, to) in [("ttft_ms", "ttftMs"), ("total_time_ms", "durationMs")] {
            if let Some(n) = u[from]
                .as_f64()
                .filter(|n| *n >= 0.0 && *n <= 9_007_199_254_740_991.0)
            {
                r["latency"][to] = json!(n);
            }
        }
    }
    if writer.provider == Provider::ForgeCode {
        if let Some(cost) = u["cost"]
            .as_f64()
            .filter(|n| *n >= 0.0 && *n <= 9_007_199_254_740_991.0)
        {
            r["cost"] = json!({"amount":cost,"currency":"USD"});
        }
    }
    if usage["input"]["total"].is_number() {
        usage["input"]["includesCache"] = if writer.provider == Provider::DeepAgents {
            json!(true)
        } else {
            Value::Null
        };
    }
    if !usage.as_object().is_none_or(|o| o.is_empty()) {
        r["usage"] = usage;
    }
    if let Some(model) = v["model"].as_str() {
        r["model"] = json!({"id":model});
    }
    if let Some(reason) = v["stopReason"].as_str() {
        r["stopReason"] = json!(reason);
    }
    values::preserve(spool, &mut r, writer.provider, "usage", u)?;
    meta(&mut r, writer.provider, "messageKey", &v["key"]);
    if writer.provider == Provider::DeepAgents && v["messageId"].is_string() {
        r["inferenceId"] = json!(format!("message:{}", v["messageId"].as_str().unwrap_or("")));
        writer.revise(
            spool,
            &format!("batch8:deepUsage:{}", v["messageId"]),
            r,
            &json!({}),
        )?;
    } else if v["inferenceId"].is_string() {
        writer.revise(
            spool,
            &format!(
                "batch8:inference:{}",
                v["inferenceId"].as_str().unwrap_or("")
            ),
            r,
            &json!({}),
        )?;
    } else {
        writer.emit(r, &json!({}))?;
    }
    Ok(())
}

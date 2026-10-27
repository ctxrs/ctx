use super::*;

fn decoded(v: &Value) -> Value {
    match v.get("message") {
        Some(Value::String(s)) => {
            document::decode(s.as_bytes()).unwrap_or_else(|_| json!({"content":s}))
        }
        Some(v) if v.is_object() => v.clone(),
        _ => v.clone(),
    }
}
pub(super) fn call_id(v: &Value) -> Option<&str> {
    if v["type"] == "function_call" {
        return v["callId"].as_str();
    }
    string(v, &["id", "tool_call_id", "toolCallId", "tool_use_id"])
}
pub(super) fn calls(v: &Value) -> Vec<Value> {
    if v["type"] == "function_call" && v["callId"].is_string() {
        return vec![v.clone()];
    }
    if let Some(states) = v["toolCallStates"].as_array() {
        return states
            .iter()
            .filter_map(|s| s.get("toolCall").cloned())
            .collect();
    }
    if v["kind"] == "ActionEvent" {
        return v.get("tool_call").cloned().into_iter().collect();
    }
    let v = decoded(v);
    let mut calls = Vec::new();
    for key in ["tool_calls", "toolCalls"] {
        if let Some(a) = v[key].as_array() {
            calls.extend(a.iter().cloned());
        }
    }
    for key in ["content", "parts"] {
        for b in v[key].as_array().into_iter().flatten() {
            if matches!(
                string(b, &["type", "kind", "part_kind"]),
                Some("tool_use" | "tool-call" | "tool_call")
            ) {
                calls.push(b.clone());
            }
        }
    }
    calls
}
pub(super) fn call(
    spool: &Spool,
    w: &mut Writer,
    row: &Value,
    c: &Value,
    model: bool,
) -> Result<String> {
    let mut v = json!({"type":"tool_call"});
    entity(&mut v, "callId", call_id(c), w);
    let name = string(&c["function"], &["name"]).or_else(|| string(c, &["name", "tool_name"]));
    if let Some(n) = name {
        v["name"] = json!(n);
    } else {
        v["capture"] = json!({"name":{"state":"absent"}});
    }
    if model {
        v["requestedBy"] = json!("model");
    }
    if let Some(input) = c
        .pointer("/function/arguments")
        .or_else(|| c.get("arguments"))
        .or_else(|| c.get("args"))
        .or_else(|| c.get("input"))
    {
        if let Some(s) = input.as_str() {
            v["inputText"] = json!(s);
        } else if !input.is_null() {
            v["input"] = input.clone();
        }
    }
    emit(spool, w, v, row)
}
pub(super) fn result(
    spool: &Spool,
    w: &mut Writer,
    row: &Value,
    id: Option<&str>,
    status: &str,
    content: Option<Vec<Value>>,
    raw: Option<&Value>,
) -> Result<String> {
    let ambiguous = if let Some(id) = id {
        spool.db.query_row(
            "SELECT count(*)>1 FROM batch_calls WHERE id=?1",
            [id],
            |r| r.get::<_, bool>(0),
        )?
    } else {
        false
    };
    let mut v = json!({"type":"tool_result","status":status,"linkage":if ambiguous{"ambiguous"}else if id.is_some(){"exact"}else{"none"}});
    if !ambiguous {
        if let Some(id) = id {
            v["callId"] = json!(id);
        }
    }
    if let Some(c) = content {
        v["content"] = json!(c);
    } else {
        v["capture"] = json!({"content":{"state":"absent"}});
    }
    if let Some(raw) = raw {
        v["raw"] = raw.clone();
        exec(&mut v, raw);
    }
    emit(spool, w, v, row)
}
pub(super) fn exec(v: &mut Value, raw: &Value) {
    let mut e = json!({});
    if let Some(code) = raw["exit_code"]
        .as_i64()
        .or_else(|| raw["exitCode"].as_i64())
        .filter(|n| *n >= i32::MIN as i64 && *n <= u32::MAX as i64)
    {
        e["exitCode"] = json!(code);
    }
    if let Some(s) = raw["signal"].as_str() {
        e["signal"] = json!(s);
    }
    if let Some(t) = raw["timed_out"]
        .as_bool()
        .or_else(|| raw["timedOut"].as_bool())
    {
        e["timedOut"] = json!(t);
    }
    if !e.as_object().is_none_or(|o| o.is_empty()) {
        v["exec"] = e;
    }
}
pub(super) fn status(n: &Value) -> &'static str {
    if n["is_error"] == true || n["isError"] == true {
        return "failed";
    }
    if n["is_error"] == false || n["isError"] == false {
        return "completed";
    }
    match string(n, &["status"]) {
        Some("done" | "completed" | "success") => "completed",
        Some("errored" | "failed" | "error") => "failed",
        Some("canceled" | "cancelled") => "cancelled",
        Some("denied") => "denied",
        Some("interrupted") => "interrupted",
        _ => "unknown",
    }
}
fn result_id(n: &Value) -> Option<&str> {
    let mut id = None;
    for v in std::iter::once(n).chain(n["parts"].as_array().into_iter().flatten()) {
        for key in [
            "tool_use_id",
            "toolUseId",
            "tool_call_id",
            "toolCallId",
            "call_id",
            "callId",
        ] {
            if let Some(next) = v[key].as_str().filter(|s| !s.is_empty()) {
                if id.is_some_and(|old| old != next) {
                    return None;
                }
                id = Some(next);
            }
        }
    }
    id
}
pub(super) fn message(
    spool: &Spool,
    w: &mut Writer,
    row: &Value,
    n: &Value,
    native: Option<&str>,
    role: Option<&str>,
) -> Result<()> {
    w.slot(0);
    let raw = n.get("content").or_else(|| n.get("parts"));
    let is_result = matches!(role, Some("tool" | "function"));
    if is_result {
        let content = raw.map(|v| {
            if let Some(blocks) = v.as_array() {
                blocks
                    .iter()
                    .flat_map(|b| {
                        if matches!(string(b, &["type", "kind"]), Some("tool_result")) {
                            parts(&b["content"], w.provider)
                        } else {
                            parts(b, w.provider)
                        }
                    })
                    .collect()
            } else {
                parts(v, w.provider)
            }
        });
        result(
            spool,
            w,
            row,
            result_id(n),
            if n.get("status").is_none() && n["parts"].as_array().is_some_and(|a| a.len() == 1) {
                status(&n["parts"][0])
            } else {
                status(n)
            },
            content,
            Some(n),
        )?;
        return Ok(());
    }
    let mut v = json!({"type":"message","content":[]});
    entity(&mut v, "messageId", native, w);
    if let Some(r) = role.filter(|r| matches!(*r, "user" | "assistant" | "system" | "developer")) {
        v["role"] = json!(r);
    } else {
        v["capture"]["role"] = json!({"state":"absent"});
    }
    if let Some(raw) = raw {
        v["content"] = json!(parts(raw, w.provider));
    } else {
        v["capture"]["content"] = json!({"state":"absent"});
    }
    if let Some(reasoning) = n["reasoning_content"].as_str() {
        if let Some(c) = v["content"].as_array_mut() {
            c.insert(
                0,
                json!({"type":"reasoning","visibility":"full","text":reasoning}),
            );
        }
    }
    emit(spool, w, v, row)?;
    for (i, c) in calls(n).iter().enumerate() {
        w.slot(10_000 + i as u64 * 10);
        call(spool, w, row, c, role == Some("assistant"))?;
    }
    for (i, b) in raw
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if matches!(
            string(b, &["type", "kind", "part_kind"]),
            Some("tool_result" | "tool-return")
        ) {
            w.slot(100_000 + i as u64);
            result(
                spool,
                w,
                row,
                string(b, &["tool_use_id", "tool_call_id"]),
                status(b),
                b.get("content").map(|v| parts(v, w.provider)),
                Some(b),
            )?;
        }
    }
    if let Some(u) = n.get("usage").filter(|u| u.is_object()) {
        w.slot(200_000);
        let mut inf = json!({"type":"inference","usage":usage(u)});
        entity(
            &mut inf,
            "inferenceId",
            string(n, &["requestId", "request_id", "response_id"]),
            w,
        );
        emit(spool, w, inf, row)?;
    }
    Ok(())
}
pub(super) fn codebuddy(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    if row["kind"] == "metadata" {
        return metadata(spool, w, row);
    }
    let n = &row["value"];
    if n["type"] == "function_call" && n["callId"].is_string() {
        return call(spool, w, row, n, true).map(|_| ());
    }
    if n["type"] == "function_call_result" && n["callId"].is_string() {
        return result(
            spool,
            w,
            row,
            n["callId"].as_str(),
            status(n),
            n.get("output").map(|v| parts(v, w.provider)),
            Some(n),
        )
        .map(|_| ());
    }
    let mut m = decoded(n);
    if m.get("_ctx_native_json").is_some() {
        return emit(
            spool,
            w,
            json!({"type":format!("{}/encoded_message",w.provider.domain())}),
            row,
        )
        .map(|_| ());
    }
    let role = string(n, &["role"])
        .or_else(|| string(&n["_ctx_reference"], &["role"]))
        .or_else(|| string(&m, &["role"]))
        .map(str::to_owned);
    if role.is_none() && m.get("content").is_none() {
        return metadata(spool, w, row);
    }
    // CLI payloads sometimes store only text; the IDE's nested JSON is retained verbatim in source metadata.
    if m.get("content").is_none() {
        if let Some(t) = m.get("text").cloned() {
            m["content"] = t;
        }
    }
    message(spool, w, row, &m, row["nativeId"].as_str(), role.as_deref())
}
pub(super) fn rovo(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    if row["kind"] == "metadata" {
        return metadata(spool, w, row);
    }
    let n = &row["value"];
    // Unqualified newer request/response layouts remain native evidence.
    if n.get("role").is_none() {
        return emit(
            spool,
            w,
            json!({"type":"com.atlassian.rovodev/message"}),
            row,
        )
        .map(|_| ());
    }
    let mut m = n.clone();
    if let Some(a) = m["parts"].as_array_mut() {
        for b in a {
            if b.get("type").is_none() {
                if let Some(kind) = b.get("kind").cloned() {
                    b["type"] = kind;
                }
            }
        }
    }
    message(
        spool,
        w,
        row,
        &m,
        row["nativeId"].as_str(),
        string(n, &["role"]),
    )
}

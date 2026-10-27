use super::*;
pub(super) fn tool(
    s: &Spool,
    w: &mut Writer,
    d: &Value,
    n: &Value,
    cwd: Option<&str>,
    slot: u64,
) -> Result<()> {
    w.slot(slot);
    let native = d["callID"].as_str().or_else(|| d["id"].as_str());
    let Some(id) = native else {
        w.vendor(n, "unidentified_tool")?;
        return Ok(());
    };
    let state = &d["state"];
    let input = state.get("input").cloned().unwrap_or(Value::Null);
    let args = if let Some(s) = input.as_str() {
        crate::tools::Arguments::Text(s.into())
    } else {
        crate::tools::Arguments::Json(input.clone())
    };
    let name = d["tool"].as_str().or_else(|| d["name"].as_str());
    let mut call = crate::tools::call(w, s, d, id, name, args, "model")?;
    copy(&mut call, "title", state, "title");
    preserve(&mut call, w, n);
    if let Some(t) = d.pointer("/time/created").and_then(millis) {
        call["ts"] = json!(t);
    }
    w.emit(call, &json!({}))?;
    let status = match state["status"].as_str() {
        Some("completed") => "completed",
        Some("error") => "failed",
        _ => return Ok(()),
    };
    let mut result = json!({"type":"tool_result","callId":id,"linkage":if s.ambiguous_call(id)?{"ambiguous"}else{"exact"},"status":status});
    let output = state.get("content").or_else(|| state.get("output"));
    if let Some(output) = output {
        result["content"] = json!(content::output(output, w.provider));
        result["raw"] = output.clone();
    } else {
        result["capture"]["content"] = json!({"state":"absent"});
    }
    if let Some(error) = state.get("error") {
        if let Some(text) = error.as_str() {
            result["error"] = json!({"message":text});
        } else if error["message"].is_string() {
            result["error"] = json!({"message":error["message"]});
        }
    }
    if let Some(raw) = state.get("result").or_else(|| state.get("structured")) {
        result["raw"] = raw.clone();
    }
    for (dest, keys) in [
        ("startedAt", ["start", "ran"]),
        ("completedAt", ["end", "completed"]),
    ] {
        if let Some(t) = keys.iter().find_map(|k| {
            state
                .get("time")
                .and_then(|t| t.get(*k))
                .or_else(|| d.get("time").and_then(|t| t.get(*k)))
                .and_then(millis)
        }) {
            result[dest] = json!(t);
        }
    }
    if name == Some("bash") {
        if let Some(command) = input["command"].as_str() {
            result["exec"]["command"] = json!(command);
        }
        if let Some(exit) = state.pointer("/metadata/exit").and_then(Value::as_i64) {
            result["exec"]["exitCode"] = json!(exit);
        }
        if let Some(dir) = input["workdir"].as_str() {
            result["exec"]["cwd"] = crate::records::path(dir, cwd);
        }
    }
    preserve(&mut result, w, n);
    let record = w.emit(result, &json!({}))?;
    if state
        .pointer("/time/compacted")
        .or_else(|| d.pointer("/time/pruned"))
        .is_some_and(|v| !v.is_null())
    {
        w.emit(json!({"type":"compaction","compactionId":format!("prune:{id}"),"idSource":"derived","kind":"prune","replaces":{"ids":[record]},"historyRetained":true}),&json!({}))?;
    }
    if let Some(child) = state.pointer("/metadata/sessionId").and_then(Value::as_str) {
        if name == Some("task") {
            w.emit(json!({"type":"subagent","childSession":crate::records::session_ref(w.provider,child),"callId":id}),&json!({}))?;
        }
    }
    Ok(())
}

pub(super) fn shell(s: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    let d = &n["data"];
    let Some(id) = d["callID"].as_str() else {
        return w.vendor(n, "shell");
    };
    let mut call = json!({"type":"tool_call","callId":id,"callIdSource":"native","requestedBy":"user","kind":"execute","capture":{"name":{"state":"absent"}}});
    copy(&mut call, "inputText", d, "command");
    preserve(&mut call, w, n);
    timestamp(&mut call, d, &n["row"]);
    w.emit(call, &json!({}))?;
    if let Some(output) = d.get("output") {
        let mut r = json!({"type":"tool_result","callId":id,"linkage":if s.ambiguous_call(id)?{"ambiguous"}else{"exact"},"status":"unknown","raw":output,"content":content::output(output,w.provider)});
        if let Some(t) = d.pointer("/time/completed").and_then(millis) {
            r["completedAt"] = json!(t);
        }
        copy(&mut r["exec"], "command", d, "command");
        preserve(&mut r, w, n);
        w.emit(r, &json!({}))?;
    }
    Ok(())
}

use super::*;
pub(super) fn convert(
    s: &mut Spool,
    w: &mut Writer,
    _pos: u64,
    v: &Value,
    _cwd: Option<&str>,
) -> Result<()> {
    let kind = v["type"].as_str().unwrap_or("unknown");
    if kind == "progress" && v["data"]["type"] == "hook_progress" {
        let d = &v["data"];
        let event = match d["hookEvent"].as_str().unwrap_or("unknown") {
            "UserPromptSubmit" => "user_prompt_submit".into(),
            "PreToolUse" => "pre_tool_use".into(),
            "PostToolUse" => "post_tool_use".into(),
            x => format!("_com.qoder.cli/{x}"),
        };
        let mut r = json!({"type":"hook","event":event});
        copy(&mut r, "name", d, "hookName");
        copy(&mut r, "command", d, "command");
        if d.get("command").is_some() {
            r["handlerKind"] = json!("command");
        }
        retain(&mut r, w.provider, v, &[]);
        emit(w, r, v)?;
        return Ok(());
    }
    let body = &v["message"]["content"];
    let has_results = body
        .as_array()
        .is_some_and(|a| a.iter().any(|b| b["type"] == "tool_result"));
    if matches!(kind, "user" | "assistant")
        && !body.is_null()
        && (has_results || v.get("toolUseResult").is_none())
    {
        blocks::message(
            s,
            w,
            v,
            body,
            v["message"]["role"].as_str().unwrap_or(kind),
            v["uuid"].as_str().or_else(|| v["id"].as_str()),
        )?;
    } else if v.get("toolUseResult").is_none()
        && !matches!(kind, "tool_result" | "tool_output" | "function_call_output")
    {
        w.vendor(v, kind)?;
    }
    if !has_results {
        if let Some(raw) = v.get("toolUseResult").or_else(|| {
            matches!(kind, "tool_result" | "tool_output" | "function_call_output").then_some(v)
        }) {
            let id = [
                "callId",
                "toolCallId",
                "call_id",
                "tool_call_id",
                "tool_use_id",
                "toolUseId",
            ]
            .iter()
            .find_map(|k| raw[*k].as_str().or_else(|| v[*k].as_str()));
            // The sidecar is UI/raw output. Only message.content is model context.
            let mut r = result(s, id, blocks::status(raw), None, w.provider)?;
            r["raw"] = raw.clone();
            if let Some(t) = raw["content"].as_str().or_else(|| raw["output"].as_str()) {
                r["displayContent"] = json!(parts(&json!(t), w.provider));
            }
            retain(&mut r, w.provider, v, &["toolUseResult"]);
            emit(w, r, v)?;
        }
    }
    Ok(())
}

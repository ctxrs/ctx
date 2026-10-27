use super::*;
pub(super) fn convert(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    let n = &row["value"];
    let event = match n["hook_event_type"].as_str() {
        Some("PreToolUse") => "pre_tool_use",
        Some("PostToolUse") => "post_tool_use",
        Some("UserPromptSubmit") => "user_prompt_submit",
        Some("SessionStart") => "session_start",
        Some("SessionEnd") => "session_end",
        Some("Stop") => "stop",
        _ => {
            return emit(
                spool,
                w,
                json!({"type":"dev.all-hands.openhands/hook"}),
                row,
            )
            .map(|_| ())
        }
    };
    let mut v = json!({"type":"hook","event":event,"handlerKind":"command"});
    for (dest, key) in [
        ("command", "hook_command"),
        ("stdout", "stdout"),
        ("stderr", "stderr"),
    ] {
        if let Some(s) = n[key].as_str() {
            v[dest] = json!(s);
        }
    }
    if let Some(code) = n["exit_code"]
        .as_i64()
        .filter(|n| *n >= i32::MIN as i64 && *n <= u32::MAX as i64)
    {
        v["exitCode"] = json!(code);
    }
    if n["blocked"] == true {
        v["status"] = json!("blocked");
    } else if let Some(ok) = n["success"].as_bool() {
        v["status"] = json!(if ok { "completed" } else { "failed" });
    }
    emit(spool, w, v, row)?;
    Ok(())
}

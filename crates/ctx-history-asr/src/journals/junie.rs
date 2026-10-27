use super::common::*;
use crate::{
    records::{copy, path, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};
pub(super) fn convert(spool: &mut Spool, w: &mut Writer, v: &Value) -> Result<()> {
    if v["kind"] == "UserPromptEvent" {
        let mut m = message(w, v, None, Some("user"), &v["prompt"]);
        // PromptEvent alone does not distinguish typed, injected or delegated input.
        m["origin"] = json!("prompt");
        emit(w, m, v)?;
        return Ok(());
    }
    let e = &v["event"]["agentEvent"];
    if v["kind"] != "SessionA2uxEvent" {
        return vendor(w, v, "event");
    }
    let step = e["stepId"].as_str();
    match e["kind"].as_str() {
        Some("ResultBlockUpdatedEvent") => {
            let mut m = message(w, v, None, None, &Value::Null);
            m["displayContent"] = json!(parts(&e["result"]));
            m["audience"] = json!("user");
            // UI updates replace the named step; they do not establish model message IDs.
            emit(w, m, v)?;
        }
        Some("LlmResponseMetadataEvent") => {
            let by_model:Vec<Value> = e["modelUsage"].as_array().into_iter().flatten().filter_map(|u| {
                let model = u["model"].as_str()?;
                Some(json!({"model":{"id":model},"usage":usage(u,["inputTokens","outputTokens","cacheInputTokens","cacheCreateTokens","totalTokens"])}))
            }).collect();
            if by_model.is_empty() {
                return vendor(w, v, "usage_event");
            }
            let mut r = json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":null,"byModel":by_model});
            retain(&mut r, w, "nativeRecord", v);
            emit(w, r, v)?;
        }
        Some("AgentFailureEvent") => {
            let mut r = json!({"type":"state","state":"error","error":{}});
            copy(&mut r["error"], "message", e, "message");
            copy(&mut r["error"], "code", e, "errorCode");
            retain(&mut r, w, "nativeRecord", v);
            emit(w, r, v)?;
        }
        Some(
            "TerminalBlockUpdatedEvent"
            | "ToolBlockUpdatedEvent"
            | "ViewFilesBlockUpdatedEvent"
            | "FileChangesBlockUpdatedEvent",
        ) => {
            let Some(step) = step else {
                return vendor(w, v, "unidentified_step");
            };
            let derived = v["_asr_step_key"].as_str().unwrap_or(step);
            let mut c = call(w, Some(derived), None, None);
            c["callIdSource"] = json!("derived");
            if e["command"].is_string() {
                c["input"] = json!({"command":e["command"]});
                c["kind"] = json!("execute");
                c.get_mut("capture")
                    .and_then(Value::as_object_mut)
                    .map(|o| o.remove("input"));
            }
            copy(&mut c, "title", e, "text");
            retain(&mut c, w, "nativeRecord", v);
            emit(w, c, v)?;
            w.slot(100);
            if e.get("details").is_some() || e.get("status").is_some() {
                let mut r = result(
                    spool,
                    w,
                    Some(derived),
                    match e["status"].as_str() {
                        Some("COMPLETED") => "completed",
                        Some("FAILED") => "failed",
                        Some("CANCELLED") => "cancelled",
                        _ => "unknown",
                    },
                )?;
                r["linkage"] = json!("derived");
                r["capture"]["content"] = json!({"state":"absent"});
                if !e["details"].is_null() {
                    r["displayContent"] = json!(parts(&e["details"]));
                }
                if let Some(command) = e["command"].as_str() {
                    r["exec"]["command"] = json!(command);
                }
                if let Some(code) = e["exitCode"]
                    .as_i64()
                    .filter(|n| (-2_147_483_648..=4_294_967_295).contains(n))
                {
                    r["exec"]["exitCode"] = json!(code);
                }
                if let Some(n) = count(&e["durationMs"]) {
                    r["durationMs"] = json!(n);
                }
                retain(&mut r, w, "nativeStep", e);
                emit(w, r, v)?;
            }
            if e["kind"] == "FileChangesBlockUpdatedEvent" {
                w.slot(200);
                let mut changes = Vec::new();
                let mut exact = true;
                for c in e["changes"].as_array().into_iter().flatten() {
                    let before = c["beforeRelativePath"].as_str();
                    let after = c["afterRelativePath"].as_str();
                    let Some(p) = after.or(before) else {
                        continue;
                    };
                    let mut change = path(p, None);
                    change["op"] = json!(match (before, after) {
                        (None, Some(_)) => "add",
                        (Some(_), None) => "delete",
                        (Some(a), Some(b)) if a != b => "move",
                        _ => "modify",
                    });
                    if change["op"] == "move" {
                        change["oldPath"] = json!(before);
                    }
                    for side in ["before", "after"] {
                        if let Some(text) = c[format!("{side}Content")]["text"].as_str() {
                            change[side] = json!({"text":text});
                        }
                    }
                    exact &= change["op"] == "delete" || change.pointer("/after/text").is_some();
                    changes.push(change);
                }
                if !changes.is_empty() {
                    let mut r = json!({"type":"file_change","changes":changes,"precision":if exact {"exact"} else {"unknown"},"status":"unknown"});
                    retain(&mut r, w, "nativeStep", e);
                    emit(w, r, v)?;
                }
            }
        }
        _ => vendor(w, v, "ui_event")?,
    }
    Ok(())
}

use super::common::*;
use crate::{
    records::{copy, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};
pub(super) fn convert(spool: &mut Spool, w: &mut Writer, v: &Value) -> Result<()> {
    let role = v["role"].as_str();
    if role == Some("tool") {
        let stored = &v["tool_result"];
        let status = if stored["cancelled"] == true {
            "cancelled"
        } else {
            "unknown"
        };
        let mut r = result(spool, w, v["tool_call_id"].as_str(), status)?;
        if !v["content"].is_null() {
            r["content"] = json!(parts(&v["content"]));
        } else {
            r["capture"]["content"] = json!({"state":"absent"});
        }
        if let Some(raw) = stored.get("output") {
            if unsafe_number(raw) {
                retain(&mut r, w, "rawJson", raw);
            } else {
                r["raw"] = raw.clone();
            }
        }
        if let Some(seconds) = stored["duration"].as_f64().filter(|s| *s >= 0.0) {
            r["durationMs"] = json!(seconds * 1000.0);
        }
        retain(&mut r, w, "nativeRecord", v);
        emit(w, r, v)?;
        return Ok(());
    }
    if !matches!(role, Some("user" | "assistant" | "system")) {
        return vendor(w, v, "record");
    }
    let mut m = message(w, v, v["message_id"].as_str(), role, &v["content"]);
    if v["injected"] == true {
        m["author"] = json!("harness");
        m["origin"] = json!("injected");
    }
    if let Some(text) = v["reasoning_content"].as_str() {
        m["content"]
            .as_array_mut()
            .map(|a| a.push(json!({"type":"reasoning","visibility":"full","text":text})));
        m.get_mut("capture")
            .and_then(Value::as_object_mut)
            .map(|o| o.remove("content"));
    }
    if let Some(display) = v.get("user_display_content").filter(|v| !v.is_null()) {
        // The source owns the UI representation; its structured variants remain
        // data parts rather than being rendered into invented display strings.
        m["displayContent"] = json!(parts(display));
        m["displayCorrespondence"] = json!("native");
    }
    if let Some(images) = v["images"].as_array() {
        for image in images {
            if let Some(path) = image
                .pointer("/source/path")
                .or(image.get("path"))
                .and_then(Value::as_str)
            {
                // Native file references are locators, never automatically opened.
                m["content"].as_array_mut().map(|a|a.push(json!({"type":"data","data":{"imagePath":path},"_meta":{"ai.mistral.vibe/image":image}})));
            } else {
                m["content"].as_array_mut().map(|a| a.push(data(image)));
            }
        }
    }
    emit(w, m, v)?;
    if v["context_boundary"] == "compaction" {
        w.slot(10_000);
        let mut r = json!({"type":"compaction","replaces":{"state":"unknown"},"summary":parts(&v["content"])});
        retain(&mut r, w, "contextBoundary", &v["context_boundary"]);
        emit(w, r, v)?;
    }
    for (n, c) in v["tool_calls"].as_array().into_iter().flatten().enumerate() {
        w.slot(20_000 + n as u64 * 10);
        let mut r = call(
            w,
            c["id"].as_str(),
            c["function"]["name"].as_str(),
            c["function"].get("arguments"),
        );
        if role == Some("assistant") {
            r["requestedBy"] = json!("model");
        }
        retain(&mut r, w, "nativeToolCall", c);
        emit(w, r, v)?;
    }
    if let Some(shell) = v.get("manual_shell").filter(|v| v.is_object()) {
        w.slot(500_000);
        let id = shell["operation_id"].as_str();
        let mut c = call(w, id, None, Some(&json!({"command":shell["command"]})));
        c["requestedBy"] = json!("user");
        c["kind"] = json!("execute");
        emit(w, c, v)?;
        let mut r = result(
            spool,
            w,
            id,
            if shell["interrupted"] == true {
                "interrupted"
            } else {
                "unknown"
            },
        )?;
        r["exec"] = json!({});
        for (dst, src) in [
            ("command", "command"),
            ("stdout", "stdout"),
            ("stderr", "stderr"),
            ("output", "output_text"),
            ("timedOut", "timed_out"),
        ] {
            copy(&mut r["exec"], dst, shell, src);
        }
        if let Some(code) = shell["exit_code"]
            .as_i64()
            .filter(|n| (-2_147_483_648..=4_294_967_295).contains(n))
        {
            r["exec"]["exitCode"] = json!(code);
        }
        if let Some(cwd) = shell["cwd"].as_str() {
            r["exec"]["cwd"] = crate::records::path(cwd, None);
        }
        copy(&mut r, "durationMs", shell, "duration_ms");
        r["capture"]["content"] = json!({"state":"absent"});
        retain(&mut r, w, "manualShell", shell);
        emit(w, r, v)?;
    }
    Ok(())
}

use crate::{
    records::{copy, meta, Writer},
    Provider, Result,
};
use serde_json::{json, Value};
pub(super) fn inference(
    spool: &crate::spool::Spool,
    writer: &mut Writer,
    v: &Value,
    first: &Value,
    id: &str,
) -> Result<()> {
    let message = &v["message"];
    let mut r = json!({"type":"inference","inferenceId":id});
    if let Some(model) = message["model"].as_str() {
        r["model"] = json!({"id":model});
    }
    copy(&mut r, "stopReason", message, "stop_reason");
    let usage = &message["usage"];
    if usage.is_object() {
        r["usage"] = tokens(usage);
    }
    if let Some(tier) = usage.get("service_tier") {
        r["params"] = json!({"serviceTier":tier});
    }
    for (source, dest) in [
        ("server_tool_use", "serverToolUse"),
        ("speed", "speed"),
        ("inference_geo", "inferenceGeo"),
    ] {
        if let Some(value) = usage.get(source) {
            meta(&mut r, Provider::Claude, dest, value);
        }
    }
    if v.get("quotaLimits").is_none() {
        if let Some(value) = v.pointer("/apiErrorParams/rate_limit_info") {
            meta(&mut r, Provider::Claude, "quotaLimits", value);
        }
    }
    for (source, dest) in [
        ("requestId", "requestId"),
        ("advisorModel", "advisorModel"),
        ("thinkingDurationMs", "thinkingDurationMs"),
        ("quotaLimits", "quotaLimits"),
    ] {
        if let Some(value) = v.get(source) {
            meta(&mut r, Provider::Claude, dest, value);
        }
    }
    if let Some(details) = message.get("stop_details") {
        meta(&mut r, Provider::Claude, "stopDetails", details);
    }
    let mut attribution = json!({});
    for (source, dest) in [
        ("attributionAgent", "agent"),
        ("attributionSkill", "skill"),
        ("attributionPlugin", "plugin"),
        ("attributionMcpServer", "mcpServer"),
        ("attributionMcpTool", "mcpTool"),
    ] {
        copy(&mut attribution, dest, v, source);
    }
    if attribution.as_object().is_some_and(|o| !o.is_empty()) {
        meta(&mut r, Provider::Claude, "attributedTo", &attribution);
    }
    super::request::inference(spool, &mut r, first, id)?;
    writer.emit(r, v)?;
    if let Some(edits) = message
        .pointer("/context_management/applied_edits")
        .filter(|e| e.as_array().is_some_and(|a| !a.is_empty()))
    {
        writer.emit(json!({"type":"compaction","compactionId":format!("prune-{id}"),"idSource":"derived","kind":"prune","replaces":{"state":"unknown"},"_meta":{"com.anthropic.claude-code/appliedEdits":edits}}),v)?;
    }
    Ok(())
}
fn tokens(usage: &Value) -> Value {
    let mut r = json!({});
    let mut input = json!({});
    let mut output = json!({});
    for (dest, source) in [
        ("uncached", "input_tokens"),
        ("cacheRead", "cache_read_input_tokens"),
        ("cacheWrite", "cache_creation_input_tokens"),
    ] {
        copy(&mut input, dest, usage, source);
    }
    let mut ttl = json!({});
    for (dest, source) in [
        ("5m", "ephemeral_5m_input_tokens"),
        ("1h", "ephemeral_1h_input_tokens"),
    ] {
        copy(&mut ttl, dest, &usage["cache_creation"], source);
    }
    if ttl.as_object().is_some_and(|o| !o.is_empty()) {
        input["cacheWriteByTtl"] = ttl;
    }
    copy(&mut output, "total", usage, "output_tokens");
    if input.as_object().is_some_and(|o| !o.is_empty()) {
        r["input"] = input;
    }
    if output.as_object().is_some_and(|o| !o.is_empty()) {
        r["output"] = output;
    }
    r
}
pub(super) fn summary(writer: &mut Writer, v: &Value) -> Result<()> {
    let mut r =
        json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null});
    if let Some(cost) = v.get("totalCostUSD").or_else(|| v.get("costUSD")) {
        r["cost"] = json!({"amount":cost,"currency":"USD","source":"harness_estimated"});
    } else {
        return writer.vendor(v, "cost-state");
    }
    meta(&mut r, Provider::Claude, "costState", v);
    writer.emit(r, v)?;
    Ok(())
}

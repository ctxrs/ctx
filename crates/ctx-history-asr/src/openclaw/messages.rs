use super::P;
use crate::{
    native as n,
    records::{copy, meta, session_ref},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};

pub(super) fn parts(v: &Value) -> Vec<Value> {
    if let Some(t) = v.as_str() {
        return vec![json!({"type":"text","text":t})];
    }
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| match p["type"].as_str() {
            Some("toolCall") => None,
            Some("thinking") => {
                let mut r = json!({"type":"reasoning"});
                if p["redacted"] == true {
                    r["visibility"] = json!("redacted");
                    if let Some(sig) = p.get("thinkingSignature") {
                        r["opaque"] = json!({"data":sig});
                    }
                } else {
                    copy(&mut r, "text", p, "thinking");
                    copy(&mut r, "signature", p, "thinkingSignature");
                }
                Some(r)
            }
            Some("text") => {
                let mut r = json!({"type":"text","text":p["text"]});
                if let Some(sig) = p.get("textSignature") {
                    meta(&mut r, P, "textSignature", sig);
                    match sig["phase"].as_str() {
                        Some("final_answer") => r["phase"] = json!("final"),
                        Some("commentary") => r["phase"] = json!("commentary"),
                        _ => {}
                    }
                }
                copy(&mut r, "annotations", p, "annotations");
                Some(r)
            }
            Some("image" | "audio" | "resource" | "resource_link") => Some(p.clone()),
            _ => Some(json!({"type":"data","data":p,"schema":"ai.openclaw/content"})),
        })
        .collect()
}
pub(super) fn participant(s: &Spool, f: &Value, channel: Option<&str>) -> Result<Option<String>> {
    let i = &f["senderIdentity"];
    let Some(id) = i["id"]
        .as_str()
        .or(i["profileId"].as_str())
        .or(f["senderId"].as_str())
    else {
        return Ok(None);
    };
    let key = json!([
        i["type"],
        i["pluginId"],
        i["domain"],
        i["idKind"],
        i["accountId"],
        channel,
        id
    ]);
    let pid = n::stable_id(&key.to_string());
    let mut p = json!({"id":pid,"ids":[{"scheme":channel.unwrap_or(if i["type"]=="profile"{"profile"}else{"openclaw"}),"value":id}]});
    copy(&mut p, "displayName", f, "senderName");
    if i["type"] == "profile" {
        p["kind"] = json!("human");
    } else if let Some(kind) = i["senderKind"]
        .as_str()
        .filter(|k| ["human", "agent", "bot", "unknown"].contains(k))
    {
        p["kind"] = json!(kind);
    }
    if f["senderIsOwner"] == true {
        p["role"] = json!("owner");
    }
    copy(&mut p["ids"][0], "basis", i, "type");
    meta(&mut p["ids"][0], P, "identity", i);
    if let Some(u) = f.get("senderUsername") {
        meta(&mut p["ids"][0], P, "senderUsername", u);
    }
    s.save(&format!("participant:{pid}"), &p)?;
    Ok(Some(pid))
}
pub(super) fn trigger(s: &Spool, e: &Value, _h: &Value) -> Result<Option<Value>> {
    let m = &e["message"];
    let provenance = &m["provenance"];
    let native = provenance["sourceTool"].as_str();
    let window = s.entity("window")?.unwrap_or(Value::Null);
    let hook = window["hook_external_content_source"].as_str();
    let mut kind = match native {
        Some("cron" | "cron_execution") => Some("schedule"),
        Some("main_session_restart_recovery") => Some("recovery"),
        Some("exec" | "agent_harness_completion") => Some("task_completion"),
        _ => None,
    };
    if kind == Some("schedule") {
        if let Some(job) = provenance["jobId"].as_str() {
            if let Some(receipt) = s.entity(&format!("heartbeat-job:{job}"))? {
                if m["timestamp"]
                    .as_i64()
                    .zip(receipt.as_i64())
                    .is_some_and(|(at, since)| at >= since)
                {
                    kind = Some("heartbeat");
                }
            }
        }
    }
    if provenance["kind"] == "inter_session" {
        kind = Some("peer");
    }
    if kind.is_none() {
        kind = hook.map(|h| if h == "gmail" { "event" } else { "webhook" });
    }
    if kind.is_none() && native.is_none() {
        return Ok(None);
    }
    let mut t = json!({});
    if let Some(k) = kind {
        t["kind"] = json!(k);
    }
    if let Some(native) = native.or(hook).or(provenance["kind"].as_str()) {
        t["native"] = json!(native);
    }
    copy(&mut t, "id", provenance, "jobId");
    copy(&mut t, "runId", provenance, "runId");
    if let Some(from) = provenance["originSessionId"].as_str() {
        t["source"] = json!({"session":session_ref(P,from)});
    }
    Ok(Some(t))
}
pub(super) fn convert(s: &Spool, h: &Value, e: &Value, root: &str) -> Result<Vec<Value>> {
    let m = &e["message"];
    let f = &m["__openclaw"];
    let provenance = &m["provenance"];
    let role = m["role"].as_str();
    if role == Some("toolResult") {
        return super::tools::result(s, h, m, false);
    }
    if m["customType"] == "openclaw.nested-tool.v1" {
        return super::tools::nested(s, h, m);
    }
    if role == Some("bashExecution") {
        return super::tools::shell(m, root);
    }
    let mirror = &m["openclawDeliveryMirror"];
    let transcript_only = (m["provider"] == "openclaw"
        && matches!(
            m["model"].as_str(),
            Some("delivery-mirror" | "gateway-injected")
        ))
        || mirror["kind"].is_string();
    let is_model = role == Some("assistant") && m["provider"] != "openclaw" && !transcript_only;
    let window = s.entity("window")?.unwrap_or(Value::Null);
    let builtin = window["agent_harness_id"].is_null() || window["agent_harness_id"] == "openclaw";
    let mut output = Vec::new();
    let mut inf = None;
    if is_model && builtin {
        let r = inference(s, m, root)?;
        inf = r["inferenceId"].as_str().map(str::to_owned);
        output.push(r);
    }
    let mut r = json!({"type":"message","messageId":root});
    if matches!(role, Some("user" | "assistant" | "system" | "developer"))
        && m["operatorMessage"] != true
    {
        r["role"] = m["role"].clone();
    } else {
        r["capture"]["role"] = json!({"state":"absent"});
    }
    if is_model {
        r["author"] = json!("model");
    }
    if provenance["kind"] == "inter_session" {
        r["author"] = json!("agent");
        r["origin"] = json!("inter_session");
    } else if provenance["kind"] == "internal_system"
        || m["synthetic"] == true
        || m["runtimeContext"] == true
    {
        r["author"] = json!("harness");
        r["origin"] = json!("injected");
    } else if role == Some("user") {
        r["origin"] = json!("prompt");
        if f["senderIdentity"]["type"] == "profile" || f["senderIdentity"]["senderKind"] == "human"
        {
            r["author"] = json!("human");
        }
    }
    if m["operatorMessage"] == true {
        r["author"] = json!("human");
    }
    if role == Some("custom") || m["provider"] == "openclaw" || transcript_only {
        r["author"] = json!(
            if m["openclawAutomation"]["kind"] == "subagent-completion" {
                "agent"
            } else {
                "harness"
            }
        );
        r["origin"] = json!(if r["author"] == "agent" {
            "subagent_report"
        } else {
            "injected"
        });
    }
    if s.entity(&format!("input-run:{}", e["id"].as_str().unwrap_or("")))?
        .is_some()
    {
        r["origin"] = json!("queued");
    }
    if f["steerTargetRunId"].is_string() {
        r["origin"] = json!("steer");
    }
    if r.get("author").is_none() {
        n::loss(s, "message.author")?;
    }
    let projection = f["modelPromptProjection"]["text"].as_str();
    let display_only = (role == Some("user")
        && m["synthetic"] != true
        && m["runtimeContext"] != true
        && projection.is_none())
        || transcript_only;
    let stored = parts(&m["content"]);
    r["content"] = json!(if display_only {
        vec![]
    } else {
        projection
            .map(|text| parts(&json!(text)))
            .unwrap_or_else(|| stored.clone())
    });
    if display_only || projection.is_some() {
        r["displayContent"] = json!(stored);
        r["displayCorrespondence"] = json!("native");
    } else if let Some(v) = m.get("openclawDisplayContent") {
        r["displayContent"] = json!(parts(v));
        r["displayCorrespondence"] = json!("native");
    }
    if display_only {
        r["capture"]["content"] = json!({"state":"absent"});
        n::loss(s, "_ai.openclaw/model_prompt")?;
    }
    if provenance["kind"] == "realtime_voice"
        || f["media"]
            .as_array()
            .is_some_and(|a| a.iter().any(|m| m["transcribed"] == true))
    {
        if let Some(a) = r["content"].as_array_mut() {
            for p in a {
                if p["type"] == "text" {
                    p["derivation"] = json!({"method":"transcription"});
                }
            }
        }
    }
    if m["excludeFromContext"] == true || transcript_only {
        r["audience"] = json!("user");
    } else if m["display"] == false || m["runtimeContext"] == true {
        r["audience"] = json!("model");
    }
    match m["stopReason"].as_str() {
        Some("aborted") => r["status"] = json!("interrupted"),
        Some("error") => r["status"] = json!("error"),
        Some("length") => r["status"] = json!("partial"),
        Some("stop" | "toolUse") if is_model => r["status"] = json!("complete"),
        _ => {}
    }
    let transport = &f["transport"];
    if let Some(from) = provenance["originSessionId"].as_str() {
        r["from"] = json!({"session":session_ref(P,from)});
    } else if let Some(pid) = participant(
        s,
        f,
        transport["channel"].as_str().or(window["channel"].as_str()),
    )? {
        r["from"] = json!({"participant":pid});
    }
    if let Some(c) = transport["conversationRef"].as_str() {
        if h["conversations"]
            .as_array()
            .is_some_and(|cs| cs.iter().any(|v| v["id"] == c))
        {
            r["conversation"] = json!(c);
        } else {
            n::loss(s, "_ai.openclaw/conversation_link")?;
        }
    }
    if let Some(id) = transport.get("messageId").filter(|v| !v.is_null()) {
        r["platform"]["messageIds"] = json!([id]);
    }
    if let Some(id) = transport
        .get("replyToId")
        .or(f.get("replyToId"))
        .filter(|v| !v.is_null())
    {
        r["platform"]["replyTo"] = id.clone();
    }
    if transport["channel"].is_string() {
        r["via"] = json!("channel");
    } else if let Some(cs) = transport["clients"].as_array().filter(|a| a.len() == 1) {
        let c = &cs[0];
        match c["id"].as_str().or(c["mode"].as_str()) {
            Some("openclaw-control-ui" | "webchat") => r["via"] = json!("web"),
            Some("openclaw-tui") => r["via"] = json!("tui"),
            Some("cli") => r["via"] = json!("cli"),
            _ => {}
        }
    }
    for key in [
        "senderIsOwner",
        "requesterOrigin",
        "humanMentions",
        "replyToPreview",
        "transport",
        "media",
        "workContext",
    ] {
        if let Some(v) = f.get(key) {
            meta(&mut r, P, key, v);
        }
    }
    for (key, dest) in [
        ("openclawDelivery", "delivery"),
        ("providerReplay", "providerReplay"),
        ("provenance", "provenance"),
        ("openclawDeliveryMirror", "deliveryMirror"),
    ] {
        if let Some(v) = m.get(key) {
            meta(&mut r, P, dest, v);
        }
    }
    let mut mirror_meta = json!({});
    for k in [
        "mirrorIdentity",
        "upstreamUserText",
        "mirrorSourceFingerprint",
    ] {
        copy(&mut mirror_meta, k, f, k);
    }
    if mirror_meta.as_object().is_some_and(|o| !o.is_empty()) {
        meta(&mut r, P, "mirror", &mirror_meta);
    }
    if let Some(original) = mirror["sourceMessageId"].as_str() {
        if let Some(m) = s.entity(&format!("message:{original}"))? {
            r["copiedFrom"] = json!({"record":m["_record"],"proof":"native_field"});
        } else {
            n::loss(s, "copies")?;
        }
    }
    if let Some(inf) = &inf {
        r["inferenceId"] = json!(inf);
    }
    // Unknown source facts remain namespaced; tool inputs are emitted separately
    // so secret arguments cannot escape inside a surrounding message's metadata.
    let mut extra = m.clone();
    if let Some(o) = extra.as_object_mut() {
        o.remove("content");
        o.remove("__openclaw");
    }
    n::retain(&mut r, P, "message", &extra);
    output.push(r);
    if is_model {
        for call in m["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "toolCall")
        {
            output.push(super::tools::call(s, call, inf.as_deref(), "model", None)?);
        }
    }
    // A bare NO_REPLY is content, not evidence of a delivery attempt (v3.1).
    if matches!(
        mirror["kind"].as_str(),
        Some("channel-final" | "channel-final-suppressed")
    ) {
        let mut delivery = json!({"type":"delivery","message":format!("messageId={root}"),"status":if mirror["kind"]=="channel-final"{"sent"}else{"suppressed"}});
        copy(&mut delivery, "reason", mirror, "reason");
        output.push(delivery);
    }
    Ok(output)
}
fn inference(s: &Spool, m: &Value, root: &str) -> Result<Value> {
    let native = m["responseId"].as_str().or(m["turnId"].as_str());
    let id = native
        .map(str::to_owned)
        .unwrap_or_else(|| format!("inference-{root}"));
    let mut r = json!({"type":"inference","inferenceId":id,"idSource":if native.is_some(){"native"}else{"derived"},"model":{}});
    copy(&mut r["model"], "provider", m, "provider");
    copy(
        &mut r["model"],
        "id",
        m,
        if m["responseModel"].is_string() {
            "responseModel"
        } else {
            "model"
        },
    );
    copy(&mut r["model"], "requested", m, "model");
    copy(&mut r, "stopReason", m, "stopReason");
    for (a, b) in [("message", "errorMessage"), ("code", "errorCode")] {
        if let Some(v) = m.get(b) {
            r["error"][a] = v.clone();
        }
    }
    let u = &m["usage"];
    if u["usageReport"]["state"] == "unavailable" {
        n::loss(s, "usage")?;
    } else if u.is_object() {
        for (a, b) in [
            ("uncached", "input"),
            ("cacheRead", "cacheRead"),
            ("cacheWrite", "cacheWrite"),
        ] {
            if let Some(v) = u.get(b) {
                r["usage"]["input"][a] = v.clone();
            }
        }
        for (a, b) in [("1h", "cacheWrite1h"), ("5m", "cacheWrite5m")] {
            if let Some(v) = u.get(b) {
                r["usage"]["input"]["cacheWriteByTtl"][a] = v.clone();
            }
        }
        if let Some(v) = u.get("output") {
            r["usage"]["output"]["total"] = v.clone();
        }
        if let Some(v) = u.get("totalTokens") {
            r["usage"]["total"] = v.clone();
        }
        if let Some(v) = u["cost"].get("total") {
            r["cost"] = json!({"amount":v,"currency":"USD","source":if u["cost"]["totalOrigin"]=="provider-billed"{"provider_reported"}else{"harness_estimated"}});
        }
    }
    meta(
        &mut r,
        P,
        "usageReport",
        &json!({"usageReport":u["usageReport"],"cacheTelemetry":u["cacheTelemetry"]}),
    );
    if let Some(cost) = u.get("cost") {
        meta(&mut r, P, "costComponents", cost);
    }
    if let Some(v) = u.get("contextUsage") {
        meta(&mut r, P, "contextUsage", v);
    }
    if let Some(old) = s.entity(&format!("inference:{id}"))? {
        r["supersedes"] = old["_record"].clone();
    }
    Ok(r)
}

use crate::{
    records::{copy, meta},
    Provider,
};
use serde_json::{json, Value};

pub fn parts(value: &Value, provider: Provider) -> Vec<Value> {
    if let Some(text) = value.as_str() {
        return vec![json!({"type":"text","text":text})];
    }
    value
        .as_array()
        .map(|a| a.iter().filter_map(|p| part(p, provider)).collect())
        .unwrap_or_default()
}
fn part(p: &Value, provider: Provider) -> Option<Value> {
    let kind = p["type"].as_str().unwrap_or("unknown");
    let mut v = match kind {
        "text" | "input_text" | "output_text" | "summary_text" => {
            json!({"type":"text","text":p["text"]})
        }
        "thinking" => {
            let mut r = json!({"type":"reasoning","visibility":"full"});
            copy(&mut r, "text", p, "thinking");
            copy(&mut r, "signature", p, "signature");
            r
        }
        "redacted_thinking" => {
            json!({"type":"reasoning","visibility":"redacted","opaque":{"format":"anthropic.redacted_thinking","data":p["data"]}})
        }
        "encrypted_content" => {
            json!({"type":"opaque","format":"openai.encrypted_content","data":p["encrypted_content"]})
        }
        "image" if p.get("source").is_some() => {
            let source = &p["source"];
            if source["type"] == "base64" {
                json!({"type":"image","mimeType":source["media_type"],"data":source["data"]})
            } else {
                json!({"type":"resource_link","name":"image","uri":source["url"]})
            }
        }
        "input_image" | "image" if p.get("image_url").is_some() => {
            let url = p["image_url"]
                .as_str()
                .or_else(|| p.pointer("/image_url/url").and_then(Value::as_str))?;
            if let Some((mime, data)) = url
                .strip_prefix("data:")
                .and_then(|s| s.split_once(";base64,"))
            {
                json!({"type":"image","mimeType":mime,"data":data})
            } else {
                json!({"type":"resource_link","name":"image","uri":url})
            }
        }
        "image" | "audio" | "resource" | "resource_link" => p.clone(),
        "refusal" => json!({"type":"refusal","text":p.get("refusal").or_else(||p.get("text"))?}),
        "tool_use" | "tool_result" | "server_tool_use" => return None,
        _ => {
            json!({"type":"data","data":p,"schema":format!("{}/content/{}",provider.domain(),kind)})
        }
    };
    for k in ["annotations", "_meta"] {
        copy(&mut v, k, p, k);
    }
    for k in ["logprobs", "detail"] {
        if let Some(x) = p.get(k) {
            meta(&mut v, provider, k, x);
        }
    }
    Some(v)
}
pub fn codex_parts(payload: &Value) -> Vec<Value> {
    let mut result = parts(&payload["content"], Provider::Codex);
    if let Some(kinds) = payload
        .pointer("/internal_chat_message_metadata_passthrough/content_item_kinds")
        .and_then(Value::as_array)
    {
        for (part, kind) in result.iter_mut().zip(kinds) {
            let kind = kind.as_str().unwrap_or("");
            let author = if matches!(kind, "user.text" | "user.image") {
                Some("human")
            } else if kind == "hooks.additional_context" {
                Some("hook")
            } else if kind == "multi_agent.subagent_notification" {
                Some("agent")
            } else if kind.ends_with(".instructions") && kind != "generic.developer_instructions"
                || kind == "environments.environment_context"
            {
                Some("harness")
            } else {
                None
            };
            if let Some(a) = author {
                part["author"] = json!(a);
            }
            if matches!(kind, "user.text" | "user.image") {
                part["origin"] = json!("prompt");
            } else if kind == "multi_agent.subagent_notification" {
                part["origin"] = json!("subagent_report");
            } else if author.is_some() || kind == "generic.developer_instructions" {
                part["origin"] = json!("injected");
            }
        }
    }
    if let Some(encrypted) = payload["encrypted_content"].as_str() {
        result.push(json!({"type":"opaque","format":"openai.encrypted_content","data":encrypted}));
    }
    result
}
pub fn capture_content(record: &mut Value, source: &Value) {
    if source.is_null() {
        record["capture"] = json!({"content":{"state":"absent"}});
    } else if source.as_str().is_some_and(|s| {
        s.contains("tokens truncated")
            || s.contains("[Output truncated")
            || s.contains("<persisted-output>")
    }) {
        record["capture"] = json!({"content":{"state":"truncated","reason":"source_truncated"}});
    }
}

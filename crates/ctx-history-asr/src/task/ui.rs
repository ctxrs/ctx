use crate::{
    content,
    records::{copy, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};

pub(super) fn usage(n: &Value) -> Option<Value> {
    let mut u = json!({});
    for (source, dest) in [
        ("tokensIn", "total"),
        ("cacheReads", "cacheRead"),
        ("cacheWrites", "cacheWrite"),
    ] {
        if let Some(v) = n[source].as_u64().filter(|n| *n <= 9_007_199_254_740_991) {
            u["input"][dest] = json!(v);
        }
    }
    if u["input"].get("total").is_some() {
        u["input"]["includesCache"] = Value::Null;
    }
    if let Some(v) = n["tokensOut"]
        .as_u64()
        .filter(|n| *n <= 9_007_199_254_740_991)
    {
        u["output"]["total"] = json!(v);
    }
    (!u.as_object().unwrap().is_empty()).then_some(u)
}

pub(super) fn convert(spool: &Spool, w: &mut Writer, n: &Value) -> Result<()> {
    let kind = n["say"]
        .as_str()
        .or_else(|| n["ask"].as_str())
        .unwrap_or("unknown");
    let parsed = n["text"]
        .as_str()
        .and_then(ctx_history_asr_core::numbers::decode);
    let mut r = match kind {
        "api_req_started" => {
            let mut r = json!({"type":"inference","inferenceId":w.id(),"idSource":"derived"});
            if let Some(p) = &parsed {
                if let Some(u) = usage(p) {
                    r["usage"] = u;
                }
                if let Some(cost) = p["cost"]
                    .as_f64()
                    .filter(|_| !crate::capture::unsafe_numbers(&p["cost"]))
                {
                    r["cost"] =
                        json!({"amount":cost,"currency":"USD","source":"harness_estimated"});
                }
            }
            r
        }
        "checkpoint_created" => {
            if let Some(hash) = n["lastCheckpointHash"]
                .as_str()
                .or_else(|| n["checkpoint"].as_str())
            {
                json!({"type":"checkpoint","kind":"shadow_git","ref":hash})
            } else {
                json!({"type":format!("{}/ui",w.provider.domain())})
            }
        }
        "tool" | "command" | "use_mcp_server" if n["type"] == "ask" => {
            let mut r = json!({"type":"permission","requestId":w.id(),"idSource":"derived"});
            copy(&mut r, "description", n, "text");
            if kind == "command" {
                if let Some(text) = n["text"].as_str() {
                    r["subject"] = json!({"command":text});
                }
            }
            r
        }
        "followup" if n["type"] == "ask" => {
            let mut r = json!({"type":"input_request","requestId":w.id(),"idSource":"derived","mode":"questions"});
            if let Some(text) = parsed
                .as_ref()
                .and_then(|p| p["question"].as_str())
                .or_else(|| n["text"].as_str())
            {
                r["questions"] = json!([{"id":"0","text":text}]);
            }
            r
        }
        "text" | "reasoning" | "completion_result" | "user_feedback" | "error" => {
            let mut r = json!({"type":"message","messageId":w.id(),"idSource":"derived","audience":"user","content":[],"capture":{"content":{"state":"absent"},"role":{"state":"absent"}},"displayContent":content::parts(&n["text"],w.provider)});
            if n["partial"] == true {
                r["status"] = json!("partial");
            }
            if n["partial"] == false {
                r["status"] = json!("complete");
            }
            r
        }
        _ => json!({"type":format!("{}/ui",w.provider.domain())}),
    };
    // Images on UI messages are their display representation, with no inferred API join.
    if let (Some(images), Some(parts)) = (
        n["images"].as_array(),
        r.get_mut("displayContent").and_then(Value::as_array_mut),
    ) {
        for image in images {
            parts.extend(content::parts(
                &json!([{"type":"input_image","image_url":image}]),
                w.provider,
            ));
        }
    }
    super::emit(spool, w, r, n)?;
    if kind != "checkpoint_created" {
        if let Some(hash) = n["lastCheckpointHash"].as_str() {
            super::emit(
                spool,
                w,
                json!({"type":"checkpoint","kind":"shadow_git","ref":hash}),
                n,
            )?;
        }
    }
    Ok(())
}

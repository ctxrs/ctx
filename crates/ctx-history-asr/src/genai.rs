//! Shared GenAI content shapes, without sharing the providers' ownership rules.
use crate::{
    records::{copy, extras, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};

pub(crate) fn parts(value: &Value, provider: Provider) -> Vec<Value> {
    let items = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(value));
    items.iter().filter_map(|p| {
        if p.is_null() || p.get("functionCall").is_some() || p.get("functionResponse").is_some() {return None}
        let mut part = if let Some(text)=p.as_str() {json!({"type":"text","text":text})}
        else if let Some(text)=p["text"].as_str() {
            if p["thought"]==true {
                if provider==Provider::Gemini {json!({"type":"reasoning","visibility":"summary","summary":[text]})}
                else {json!({"type":"reasoning","text":text})}
            } else {json!({"type":"text","text":text})}
        } else if p["thoughtSignature"].is_string() {json!({"type":"reasoning","visibility":"omitted"})}
        else if let Some(mime)=p.pointer("/inlineData/mimeType").and_then(Value::as_str).filter(|s|s.starts_with("image/")||s.starts_with("audio/")) {
            let mut v=json!({"type":if mime.starts_with("image/"){"image"}else{"audio"},"mimeType":mime});
            copy(&mut v,"data",&p["inlineData"],"data");v
        } else if let Some(uri)=p.pointer("/fileData/fileUri").and_then(Value::as_str) {
            let mut v=json!({"type":"resource_link","uri":uri,"name":uri});
            copy(&mut v,"mimeType",&p["fileData"],"mimeType");v
        } else {json!({"type":"data","data":p,"schema":format!("{}/part",provider.domain())})};
        if part["type"]=="reasoning" {copy(&mut part,"signature",p,"thoughtSignature");}
        else if let Some(signature) = p.get("thoughtSignature") {meta(&mut part,provider,"thoughtSignature",signature);}
        if part["type"]!="data" { extras(&mut part,provider,p,&["text","thought","thoughtSignature","inlineData","fileData"]); }
        if matches!(part["type"].as_str(),Some("image"|"audio")) {extras(&mut part,provider,&p["inlineData"],&["mimeType","data"]);}
        if part["type"]=="resource_link" {extras(&mut part,provider,&p["fileData"],&["mimeType","fileUri"]);}
        Some(part)
    }).collect()
}

/// Skip exact repeats, keep full replacements, and retain a revision's original branch.
pub(crate) fn revision(
    spool: &Spool,
    w: &mut Writer,
    key: &str,
    mut v: Value,
    source: &Value,
) -> Result<String> {
    if let Some(previous) = spool.entity(key)? {
        if let Some(branch) = previous.get("branchesFrom") {
            v["branchesFrom"] = branch.clone();
        }
        let mut old = previous.clone();
        old.as_object_mut().unwrap().remove("_record");
        old.as_object_mut().unwrap().remove("supersedes");
        if old == v {
            return Ok(previous["_record"].as_str().unwrap().to_owned());
        }
    }
    let new_node = crate::records::context(&v) && spool.entity(key)?.is_none();
    let id = w.revise(spool, key, v, source)?;
    if new_node && w.provider == Provider::Gemini {
        if let Some(owner) = spool
            .entity("gemini-current")?
            .and_then(|v| v.as_str().map(str::to_owned))
        {
            spool.db.execute(
                "INSERT OR IGNORE INTO gemini_nodes VALUES(?1,?2)",
                rusqlite::params![owner, id],
            )?;
        }
    }
    Ok(id)
}

pub(crate) fn call(v: &Value, derived: &str, provider: Provider) -> Value {
    let id = v["id"].as_str().unwrap_or(derived);
    let mut r = json!({"type":"tool_call","callId":id,"callIdSource":if v["id"].is_string(){"native"}else{"derived"},"requestedBy":"model"});
    if v["name"].is_string() {
        copy(&mut r, "name", v, "name");
    } else {
        r["capture"] = json!({"name":{"state":"absent"}});
    }
    copy(&mut r, "input", v, "args");
    copy(&mut r, "title", v, "displayName");
    extras(
        &mut r,
        provider,
        v,
        &[
            "id",
            "name",
            "args",
            "displayName",
            "result",
            "resultDisplay",
            "status",
        ],
    );
    r
}

pub(crate) fn result(
    spool: &Spool,
    call: Option<&str>,
    content: Option<&Value>,
    native: &Value,
    provider: Provider,
) -> Result<Value> {
    let mut r = json!({"type":"tool_result","linkage":"none","status":match native["status"].as_str(){Some("success")=>"completed",Some("error")=>"failed",Some("cancelled")=>"cancelled",_=>"unknown"}});
    if let Some(id) = call {
        r["callId"] = json!(id);
        r["linkage"] = json!(if spool.ambiguous_call(id)? {
            "ambiguous"
        } else {
            "exact"
        });
    }
    if let Some(content) = content.filter(|v| !v.is_null()) {
        let mut normalized = Vec::new();
        for p in content
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| std::slice::from_ref(content))
        {
            if let Some(response) = p.get("functionResponse") {
                if let Some(data) = response.get("response") {
                    let mut part = json!({"type":"data","data":data,"schema":format!("{}/functionResponse",provider.domain())});
                    extras(&mut part, provider, response, &["response"]);
                    extras(&mut part, provider, p, &["functionResponse"]);
                    normalized.push(part);
                }
            } else {
                normalized.extend(parts(p, provider));
            }
        }
        r["content"] = json!(normalized);
        if provider == Provider::Gemini && masked(content) {
            r["capture"] = json!({"content":{"state":"masked"}});
        }
    } else {
        r["capture"] = json!({"content":{"state":"absent"}});
    }
    if let Some(display) = native.get("resultDisplay").filter(|v| !v.is_null()) {
        r["displayContent"] = json!(parts(display, provider));
        if display["truncatedForSession"] == true {
            r["capture"]["displayContent"] =
                json!({"state":"truncated","reason":"source_truncated"});
        }
    }
    // Metadata on a response is distinct from its model-facing payload.
    extras(&mut r, provider, native, &["result", "resultDisplay"]);
    Ok(r)
}
fn masked(v: &Value) -> bool {
    // The owning masking service's exact marker, only inside function responses.
    v.as_array().into_iter().flatten().any(|p| {
        p.pointer("/functionResponse/response/output")
            .and_then(Value::as_str)
            .is_some_and(|s| {
                s.starts_with("<tool_output_masked>") && s.ends_with("</tool_output_masked>")
            })
    })
}

pub(crate) fn usage(spool: &Spool, w: &mut Writer, v: &Value, id: &str) -> Result<()> {
    let gemini = w.provider == Provider::Gemini;
    let tokens = &v[if gemini { "tokens" } else { "usageMetadata" }];
    if !tokens.is_object() {
        return Ok(());
    }
    let mut usage = json!({});
    let fields = if gemini {
        ["input", "cached", "output", "thoughts", "total"]
    } else {
        [
            "promptTokenCount",
            "cachedContentTokenCount",
            "candidatesTokenCount",
            "thoughtsTokenCount",
            "totalTokenCount",
        ]
    };
    if let Some(input) = tokens[fields[0]].as_u64() {
        usage["input"] =
            json!({"total":input,"includesCache":if gemini {json!(true)}else{Value::Null}});
    }
    if let Some(cached) = tokens[fields[1]].as_u64() {
        usage["input"]["cacheRead"] = json!(cached);
    }
    let output = tokens[fields[2]].as_u64();
    let thoughts = tokens[fields[3]].as_u64();
    if let Some(thoughts) = thoughts {
        usage["output"]["reasoning"] = json!(thoughts);
    }
    if gemini {
        if let (Some(out), Some(reasoning)) = (output, thoughts) {
            if let Some(sum) = out.checked_add(reasoning) {
                usage["output"]["total"] = json!(sum);
            }
        }
    } else if thoughts == Some(0) {
        if let Some(out) = output {
            usage["output"]["total"] = json!(out);
        }
    }
    copy(&mut usage, "total", tokens, fields[4]);
    let mut r = json!({"type":"inference","inferenceId":format!("usage:{id}"),"idSource":"derived","usage":usage});
    if let Some(model) = v["model"].as_str() {
        r["model"] = json!({"requested":model});
    }
    meta(&mut r, w.provider, "usage", tokens);
    revision(spool, w, &format!("usage:{id}"), r, &json!({}))?;
    Ok(())
}

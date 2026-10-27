mod audit;
mod lifecycle;
use super::*;
use rusqlite::OptionalExtension;

pub(super) fn convert(
    s: &mut Spool,
    w: &mut Writer,
    _pos: u64,
    v: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let d = &v["data"];
    let kind = v["type"].as_str().unwrap_or("unknown");
    if let Some(turn) = d["turnId"].as_str() {
        w.turn = Some(turn.to_owned());
    } else if kind == "user.message" {
        w.turn = None;
    }
    match kind {
        "user.message" | "assistant.message" => message(s, w, v)?,
        "tool.execution_start" => {
            if d["toolCallId"]
                .as_str()
                .map(|id| prefer_start(s, id))
                .transpose()?
                .unwrap_or(true)
            {
                let request = if let Some(id) = d["toolCallId"].as_str() {
                    if !s.ambiguous_call(id)? {
                        let pos:Option<u64>=s.db.query_row("SELECT position FROM journal_calls WHERE kind='request' AND call_id=?1",[id],|r|r.get(0)).optional()?;
                        pos.map(|p| s.get(p / crate::records::STRIDE)).transpose()?
                    } else {
                        None
                    }
                } else {
                    None
                };
                call(
                    w,
                    v,
                    d,
                    request
                        .as_ref()
                        .and_then(|r| r["data"]["apiCallId"].as_str()),
                    request.is_some(),
                )?;
            } else {
                w.vendor(v, "execution_start")?;
            }
        }
        "tool.execution_complete" => {
            let status = match d["success"].as_bool() {
                Some(true) => "completed",
                Some(false) => "failed",
                None => "unknown",
            };
            let out = &d["result"];
            let mut r = result(
                s,
                d["toolCallId"].as_str(),
                status,
                out.get("content"),
                w.provider,
            )?;
            if let Some(text) = out.get("detailedContent") {
                r["displayContent"] = json!(parts(text, w.provider));
            }
            if !out.is_null() {
                r["raw"] = out.clone();
            }
            if let Some(binary) = out["binaryResultsForLlm"].as_array() {
                for b in binary {
                    if b["data"].is_string() && b["mimeType"].is_string() {
                        let p = if b["type"] == "image" {
                            json!({"type":"image","data":b["data"],"mimeType":b["mimeType"]})
                        } else {
                            json!({"type":"data","data":b})
                        };
                        r["content"].as_array_mut().map(|a| a.push(p));
                    } else {
                        r["capture"]["content"] =
                            json!({"state":"truncated","reason":"source_omitted"});
                    }
                }
            }
            if let Some(code) = d["shellExecution"]["exitCode"].as_i64() {
                r["exec"]["exitCode"] = json!(code);
            }
            if let Some(error) = d.get("error") {
                r["error"] = error.clone();
            }
            retain(&mut r, w.provider, v, &[]);
            emit(w, r, v)?;
        }
        "assistant.usage" => {
            let mut r = json!({"type":"inference"});
            entity(
                &mut r,
                "inferenceId",
                d["apiCallId"].as_str(),
                format!("inference:{}", w.id()),
            );
            if let Some(model) = d["model"].as_str() {
                r["model"]["id"] = json!(model);
            }
            if let Some(provider) = d["modelProvider"].as_str() {
                r["model"]["provider"] = json!(provider);
            }
            for (to, from) in [
                ("total", "inputTokens"),
                ("cacheRead", "cacheReadTokens"),
                ("cacheWrite", "cacheWriteTokens"),
            ] {
                if let Some(x) = d.get(from) {
                    r["usage"]["input"][to] = x.clone();
                }
            }
            if d.get("inputTokens").is_some() {
                r["usage"]["input"]["includesCache"] = Value::Null;
            }
            if let Some(x) = d.get("outputTokens") {
                r["usage"]["output"]["total"] = x.clone();
            }
            if let Some(x) = d.get("duration") {
                r["latency"]["durationMs"] = x.clone();
            }
            if let Some(x) = d.get("outputTtftMs") {
                r["latency"]["ttftMs"] = x.clone();
            }
            if let Some(x) = d.get("reasoningEffort") {
                r["params"]["effort"] = x.clone();
            }
            copy(&mut r, "stopReason", d, "finishReason");
            retain(&mut r, w.provider, v, &[]);
            let key = format!("copilot:inference:{}", r["inferenceId"].as_str().unwrap());
            w.revise(s, &key, r, v)?;
        }
        _ => {
            if !audit::convert(s, w, v)? {
                lifecycle::convert(s, w, v, cwd)?;
            }
        }
    }
    if let Some(turn) = w.turn.as_deref() {
        s.save(&format!("copilot:turn:{turn}"), &json!(true))?;
    }
    if matches!(
        kind,
        "assistant.turn_end" | "session.idle" | "session.shutdown"
    ) {
        w.turn = None;
    }
    Ok(())
}
fn message(s: &mut Spool, w: &mut Writer, v: &Value) -> Result<()> {
    let d = &v["data"];
    let user = v["type"] == "user.message";
    if !user {
        if let Some(id) = d["apiCallId"].as_str() {
            let key = format!("copilot:inference:{id}");
            if s.entity(&key)?.is_none() {
                let mut inf = json!({"type":"inference","inferenceId":id,"idSource":"native"});
                if let Some(model) = d["model"].as_str() {
                    inf["model"]["id"] = json!(model);
                }
                retain(
                    &mut inf,
                    w.provider,
                    d,
                    &["content", "toolRequests", "reasoningText"],
                );
                w.revise(s, &key, inf, v)?;
            }
        }
    }
    let mut r = json!({"type":"message","role":if user{"user"}else{"assistant"}});
    entity(
        &mut r,
        "messageId",
        d["messageId"].as_str().or_else(|| v["id"].as_str()),
        format!("message:{}", w.id()),
    );
    if !user {
        r["author"] = json!("model");
    }
    let body = d.get("transformedContent").or_else(|| d.get("content"));
    if let Some(body) = body {
        r["content"] = json!(parts(body, w.provider));
    } else {
        r["capture"]["content"] = json!({"state":"absent"});
    }
    if d.get("transformedContent").is_some() {
        r["displayContent"] = json!(parts(&d["content"], w.provider));
        r["displayCorrespondence"] = json!("native");
    }
    if !user {
        r["content"] = json!(fold::copilot_content(d));
    }
    if let Some(inference) = d["apiCallId"].as_str() {
        r["inferenceId"] = json!(inference);
    }
    if user {
        lifecycle::trigger(s, w, &mut r, d)?;
    }
    retain(&mut r, w.provider, v, &[]);
    if !user && d["chunkIndex"].is_number() && d["messageId"].is_string() {
        if let Some(folded) = fold::message(s, w, d["messageId"].as_str().unwrap())? {
            r.as_object_mut().unwrap().remove("content");
            r.as_object_mut()
                .unwrap()
                .extend(folded.as_object().unwrap().clone());
            emit(w, r, v)?;
        } else {
            w.vendor(v, "response_part")?;
        }
    } else {
        emit(w, r, v)?;
    }
    w.slot(100);
    for req in d["toolRequests"].as_array().into_iter().flatten() {
        let Some(id) = req["toolCallId"].as_str() else {
            continue;
        };
        let start = prefer_start(s, id)?;
        if !start {
            call(w, v, req, d["apiCallId"].as_str(), true)?;
        }
    }
    Ok(())
}
fn call(
    w: &mut Writer,
    v: &Value,
    d: &Value,
    inference: Option<&str>,
    model_request: bool,
) -> Result<()> {
    let mut r = json!({"type":"tool_call"});
    entity(
        &mut r,
        "callId",
        d["toolCallId"].as_str(),
        format!("call:{}", w.id()),
    );
    if let Some(name) = d["toolName"].as_str().or_else(|| d["name"].as_str()) {
        r["name"] = json!(name);
        blocks::identity(&mut r, w.provider, d, name);
    } else {
        r["capture"]["name"] = json!({"state":"absent"});
    }
    input(&mut r, d.get("arguments"));
    copy(&mut r, "title", d, "title");
    if model_request {
        r["requestedBy"] = json!("model");
    } else if d["isUserRequested"] == true {
        r["requestedBy"] = json!("user");
    }
    if let Some(inference) = inference {
        r["inferenceId"] = json!(inference);
    }
    retain(&mut r, w.provider, v, &[]);
    emit(w, r, v)?;
    Ok(())
}

fn prefer_start(s: &Spool, id: &str) -> Result<bool> {
    Ok(s.db.query_row("SELECT count(CASE WHEN kind='start' THEN 1 END)>=count(CASE WHEN kind='request' THEN 1 END) FROM journal_calls WHERE call_id=?1",[id],|r|r.get(0))?)
}

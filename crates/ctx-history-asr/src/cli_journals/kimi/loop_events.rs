use super::*;
pub(super) fn convert(s: &mut Spool, w: &mut Writer, v: &Value) -> Result<()> {
    let e = &v["event"];
    if let Some(turn) = e["turnId"].as_str() {
        w.turn = Some(turn.to_owned());
    }
    let step = e["turnId"]
        .as_str()
        .zip(e["step"].as_u64())
        .map(|(t, n)| format!("{t}.{n}"));
    let request = step
        .as_deref()
        .map(|k| s.entity(&format!("kimi:request:{k}")))
        .transpose()?
        .flatten();
    let kind = e["type"].as_str().unwrap_or("unknown");
    let mut r = match kind {
        "step.begin" => json!({"type":"state","state":"running"}),
        "step.end" => {
            // One end describes the final request attempt of this native turn/step.
            let mut r=request.unwrap_or_else(||json!({"type":"inference","inferenceId":format!("step:{}",e["uuid"].as_str().unwrap_or(&w.id())),"idSource":"derived","idScope":"capture"}));
            let usage = &e["usage"];
            for (to, from) in [
                ("uncached", "inputOther"),
                ("cacheRead", "inputCacheRead"),
                ("cacheWrite", "inputCacheCreation"),
            ] {
                if let Some(x) = usage.get(from) {
                    r["usage"]["input"][to] = x.clone();
                }
            }
            if let Some(x) = usage.get("output") {
                r["usage"]["output"]["total"] = x.clone();
            }
            copy(&mut r, "stopReason", e, "rawFinishReason");
            if let Some(x) = e.get("llmFirstTokenLatencyMs") {
                r["latency"]["ttftMs"] = x.clone();
            }
            retain(&mut r, w.provider, v, &[]);
            let key = format!("kimi:inference:{}", r["inferenceId"].as_str().unwrap());
            w.revise(s, &key, r, v)?;
            return Ok(());
        }
        "content.part" => {
            if let Some(step) = e["stepUuid"].as_str() {
                let Some(mut r) = fold::message(s, w, step)? else {
                    w.vendor(v, "response_part")?;
                    return Ok(());
                };
                entity(&mut r, "messageId", None, format!("step:{step}"));
                r
            } else {
                w.vendor(v, "content.part")?;
                return Ok(());
            }
        }
        "tool.call" => {
            let mut r = json!({"type":"tool_call","requestedBy":"model"});
            entity(
                &mut r,
                "callId",
                e["toolCallId"].as_str(),
                format!("call:{}", w.id()),
            );
            copy(&mut r, "name", e, "toolName");
            copy(&mut r, "name", e, "name");
            if r.get("name").is_none() {
                r["capture"]["name"] = json!({"state":"absent"});
            }
            input(&mut r, e.get("args").or_else(|| e.get("input")));
            copy(&mut r, "title", e, "description");
            r
        }
        "tool.result" => {
            let output = e.get("result").unwrap_or(e);
            // The current harness adds status wrappers at request projection.
            // Retain the persisted representation without inventing those bytes.
            let mut r = result(
                s,
                e["toolCallId"].as_str(),
                if output["isError"] == true {
                    "failed"
                } else if e.get("result").is_some() {
                    "completed"
                } else {
                    "unknown"
                },
                None,
                w.provider,
            )?;
            r["raw"] = output.clone();
            if let Some(body) = output.get("output") {
                r["displayContent"] = json!(parts(body, w.provider));
            }
            if let Some(message) = output["message"].as_str() {
                r["displayContent"]
                    .as_array_mut()
                    .map(|a| a.push(json!({"type":"text","text":message})));
            }
            if output["truncated"] == true {
                r["capture"]["displayContent"] =
                    json!({"state":"truncated","reason":"source_truncated"});
            }
            r
        }
        _ => {
            w.vendor(v, kind)?;
            return Ok(());
        }
    };
    if let Some(request) = request {
        copy(&mut r, "inferenceId", &request, "inferenceId");
    }
    retain(&mut r, w.provider, v, &[]);
    emit(w, r, v)?;
    Ok(())
}

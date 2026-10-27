use super::*;
pub(super) fn header(spool: &Spool, w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let h = &v["data"]["header"];
    let c = &h["config"];
    let mut r = json!({"type":"config_change","snapshot":true,"options":{}});
    if c["model"].is_string() {
        r["model"] = json!({"id":c["model"]});
        copy(&mut r["model"], "provider", c, "provider");
    }
    copy(&mut r, "effort", c, "reasoningEffort");
    extras(&mut r, P, c, &["model", "provider", "reasoningEffort"]);
    extras(&mut r, P, h, &["config", "system", "tools"]);
    native(&mut r, v, &["header"]);
    w.emit(r, time)?;
    let mut instructions = Vec::new();
    if let Some(system) = h["system"].as_str() {
        instructions.push(w.emit(json!({"type":"instructions","kind":"system_prompt","scope":"session","content":[{"type":"text","text":system}]}),time)?);
    }
    if let Some(tools) = h["tools"].as_array() {
        let definitions: Vec<_> = tools
            .iter()
            .map(|t| {
                let mut def = json!({"name":t["name"]});
                copy(&mut def, "description", t, "description");
                copy(&mut def, "inputSchema", t, "parameters");
                extras(&mut def, P, t, &["name", "description", "parameters"]);
                def
            })
            .collect();
        instructions.push(w.emit(json!({"type":"instructions","kind":"tool_definitions","scope":"session","tools":definitions}),time)?);
    }
    spool.save(
        "deepseek:request",
        &json!({"config":c,"instructions":instructions}),
    )?;
    Ok(())
}
pub(super) fn inference(spool: &Spool, w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let d = &v["data"];
    let m = &d["message"];
    let s = &m["source"];
    let Some(id) = m["id"].as_str() else {
        return Ok(());
    };
    let mut r = json!({"type":"inference","inferenceId":self::id(spool,id)?,"idSource":"derived","purpose":"turn","model":{}});
    copy(&mut r["model"], "provider", s, "provider");
    copy(&mut r["model"], "id", s, "model");
    if d["usage"].is_object() {
        r["usage"] = jsonl::usage(&d["usage"], false);
        meta(&mut r, P, "usage", &d["usage"]);
    } else if let Some(usage) = spool.entity("deepseek:usage")?.filter(Value::is_object) {
        r["usage"] = jsonl::usage(&usage, false);
        meta(&mut r, P, "usage", &usage);
    }
    if let Some(finish) = spool
        .entity("deepseek:finish")?
        .filter(|f| f["inferenceId"] == r["inferenceId"])
    {
        copy(&mut r, "stopReason", &finish, "stopReason");
        copy(&mut r, "error", &finish, "error");
    }
    if let Some(req) = spool.entity("deepseek:request")? {
        let config = &req["config"];
        copy(&mut r["model"], "requested", config, "model");
        r["params"] = json!({});
        for (a, b) in [
            ("effort", "reasoningEffort"),
            ("maxOutputTokens", "maxTokens"),
            ("temperature", "temperature"),
            ("topP", "topP"),
        ] {
            copy(&mut r["params"], a, config, b);
        }
        r["context"] = json!({"instructions":req["instructions"],"complete":false});
        extras(
            &mut r,
            P,
            config,
            &[
                "provider",
                "model",
                "reasoningEffort",
                "maxTokens",
                "temperature",
                "topP",
            ],
        );
    }
    for field in ["turn", "step"] {
        if let Some(value) = d.get(field) {
            meta(&mut r, P, field, value);
        }
    }
    // The assembled message is one step response; stream deltas are retained separately as vendor data.
    let key = format!(
        "deepseek:inference:{}",
        r["inferenceId"].as_str().unwrap_or("")
    );
    w.revise(spool, &key, r, time)?;
    Ok(())
}
pub(super) fn summary(_spool: &Spool, w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let d = &v["data"];
    let mut r = json!({"type":"inference","inferenceId":format!("compaction:{}",d["compactionId"].as_str().unwrap_or("unknown")),"idSource":"derived","purpose":"compaction","model":{"provider":d["provider"],"id":d["model"]}});
    if d["usage"].is_object() {
        r["usage"] = jsonl::usage(&d["usage"], false);
    }
    if let Some(max) = d["maxTokens"].as_u64() {
        r["params"] = json!({"maxOutputTokens":max});
    }
    native(&mut r, v, &["provider", "model", "usage", "maxTokens"]);
    w.emit(r, time)?;
    Ok(())
}

// Assistant blocks are the request authority. A tool/call event records execution start;
// it does not add another model request with the same identity.
pub(super) fn call(
    spool: &Spool,
    w: &mut Writer,
    v: &Value,
    block: &Value,
    time: &Value,
    response: Option<&str>,
) -> Result<Option<String>> {
    let Some(id) = block["id"].as_str().or_else(|| block["callId"].as_str()) else {
        w.vendor(block, "unidentified_call")?;
        return Ok(None);
    };
    let args = block["arguments"].as_str().unwrap_or("");
    let argument = ctx_history_capture_model::exact_json_value(args)
        .map(tools::Arguments::Json)
        .unwrap_or_else(|| tools::Arguments::Text(args.into()));
    let mut r = tools::call(w, spool, v, id, block["name"].as_str(), argument, "model")?;
    if let Some(response) = response {
        r["inferenceId"] = json!(self::id(spool, response)?);
    }
    extras(&mut r, P, block, &["id", "callId", "name", "arguments"]);
    let record = w.emit(r, time)?;
    spool.save(
        &format!(
            "deepseek:call:{}:{}:{id}",
            v["data"]["turn"], v["data"]["step"]
        ),
        &json!(record),
    )?;
    Ok(Some(record))
}
pub(super) fn call_started(spool: &Spool, w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let d = &v["data"];
    let Some(id) = d["callId"].as_str() else {
        return w.vendor(v, "unidentified_call");
    };
    if spool
        .entity(&format!("deepseek:call:{}:{}:{id}", d["turn"], d["step"]))?
        .is_none()
    {
        call(spool, w, v, d, time, None)?;
    }
    w.vendor(v, "tool_started")
}

pub(super) fn id(spool: &Spool, message: &str) -> Result<String> {
    Ok(spool
        .entity("deepseek:response")?
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("response:{message}")))
}
pub(super) fn chunk(spool: &Spool, w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let c = &v["data"]["chunk"];
    if c["type"] != "finish"
        && spool
            .entity("deepseek:response")?
            .is_some_and(|v| !v.is_null())
    {
        spool.save("deepseek:response", &Value::Null)?;
        spool.save("deepseek:usage", &Value::Null)?;
    }
    if c["type"] == "usage" {
        spool.save("deepseek:usage", &c["usage"])?;
    } else if c["type"] == "finish" {
        let id = format!("request:{}", v["seq"]);
        spool.save("deepseek:response", &json!(id))?;
        let mut r =
            json!({"type":"inference","inferenceId":id,"idSource":"derived","purpose":"turn"});
        if let Some(usage) = spool.entity("deepseek:usage")?.filter(Value::is_object) {
            r["usage"] = jsonl::usage(&usage, false);
            meta(&mut r, P, "usage", &usage);
        }
        if let Some(reason) = c["reason"]["kind"].as_str() {
            r["stopReason"] = json!(match reason {
                "stop" => "end_turn",
                "tool-calls" => "tool_use",
                "length" => "max_tokens",
                "aborted" => "cancelled",
                "error" => "error",
                _ => reason,
            });
        }
        if let Some(error) = c["reason"].get("error") {
            r["error"] = json!({});
            copy(&mut r["error"], "message", error, "message");
            copy(&mut r["error"], "code", error, "code");
        }
        if let Some(req) = spool.entity("deepseek:request")? {
            r["model"] = json!({});
            copy(&mut r["model"], "provider", &req["config"], "provider");
            copy(&mut r["model"], "requested", &req["config"], "model");
            r["context"] = json!({"instructions":req["instructions"],"complete":false});
        }
        native(&mut r, v, &[]);
        spool.save("deepseek:finish", &r)?;
        w.revise(spool, &format!("deepseek:inference:{id}"), r, time)?;
    }
    w.vendor(v, "assistant_chunk")
}

use super::*;

pub(super) fn continue_item(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    if row["kind"] == "metadata" {
        return metadata(spool, w, row);
    }
    let n = &row["value"];
    let mut m = n["message"].clone();
    if !m.is_object() {
        return emit(spool, w, json!({"type":"dev.continue/history_item"}), row).map(|_| ());
    }
    // toolCallStates is the authoritative copy of the calls attached to the assistant item.
    if let Some(states) = n["toolCallStates"].as_array() {
        m["toolCalls"] = json!(states
            .iter()
            .filter_map(|s| s.get("toolCall"))
            .collect::<Vec<_>>());
    }
    if let Some(text) = n.pointer("/reasoning/text").and_then(Value::as_str) {
        m["reasoning_content"] = json!(text);
    }
    if m["role"] == "thinking" {
        m["role"] = json!("assistant");
        if let Some(text) = m["content"].as_str() {
            let mut part = json!({"type":"thinking","thinking":text});
            if let Some(signature) = m["signature"].as_str() {
                part["signature"] = json!(signature);
            }
            m["content"] = json!([part]);
        }
    }
    messages::message(spool, w, row, &m, m["id"].as_str(), m["role"].as_str())?;
    // Output ContextItems are UI/tool state, not proof of the rendered model input.
    // Explicit role=tool history messages are projected independently above.
    for (i, state) in n["toolCallStates"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if state.get("output").is_some()
            || matches!(
                state["status"].as_str(),
                Some("done" | "errored" | "canceled")
            )
        {
            w.slot(300_000 + i as u64);
            let id = state["toolCallId"].as_str();
            // An explicit tool message is the model-facing result; keep the state only in source metadata.
            let found = spool
                .entity(&format!("continue-result:{}", id.unwrap_or("")))?
                .is_some();
            if !found {
                messages::result(
                    spool,
                    w,
                    row,
                    id,
                    messages::status(state),
                    None,
                    Some(state),
                )?;
            }
        }
    }
    for (i, log) in n["promptLogs"].as_array().into_iter().flatten().enumerate() {
        w.slot(400_000 + i as u64);
        let mut v = json!({"type":"inference"});
        entity(&mut v, "inferenceId", None, w);
        let mut model = json!({});
        if let Some(provider) = log["modelProvider"].as_str() {
            model["provider"] = json!(provider);
        }
        if model.as_object().is_some_and(|o| !o.is_empty()) {
            v["model"] = model;
        }
        meta(&mut v, w.provider, "promptLog", log);
        emit(spool, w, v, row)?;
    }
    if let Some(summary) = n["conversationSummary"].as_str() {
        w.slot(500_000);
        let mut v = json!({"type":"compaction","kind":"summary","replaces":{"throughId":w.tip},"summary":[{"type":"text","text":summary}]});
        entity(&mut v, "compactionId", None, w);
        emit(spool, w, v, row)?;
    }
    Ok(())
}
fn node_parts(nodes: &Value, p: Provider) -> Vec<Value> {
    nodes
        .as_array()
        .into_iter()
        .flatten()
        .map(|node| {
            if let Some(text) = node
                .pointer("/text_node/content")
                .or_else(|| node.pointer("/textNode/content"))
                .and_then(Value::as_str)
            {
                json!({"type":"text","text":text})
            } else {
                json!({"type":"data","data":node,"schema":format!("{}/node",p.domain())})
            }
        })
        .collect()
}
pub(super) fn auggie(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    if row["kind"] == "metadata" {
        return metadata(spool, w, row);
    }
    let n = &row["value"];
    let e = n.get("exchange").unwrap_or(n);
    let response = e.get("response_text").or_else(|| e.get("responseText"));
    if n["isHistorySummary"] == true {
        w.slot(0);
        let mut v = json!({"type":"compaction","kind":"summary","replaces":{"state":"unknown"}});
        entity(&mut v, "compactionId", None, w);
        if let Some(text) = response.and_then(Value::as_str) {
            v["summary"] = json!([{"type":"text","text":text}]);
        } else {
            v["capture"] = json!({"summary":{"state":"absent"}});
        }
        emit(spool, w, v, row)?;
        return Ok(());
    }
    let mut emitted = false;
    for (i, role, text, nodes) in [
        (
            0,
            "user",
            e.get("request_message").or_else(|| e.get("requestMessage")),
            e.get("request_nodes").or_else(|| e.get("requestNodes")),
        ),
        (
            1,
            "assistant",
            response,
            e.get("response_nodes").or_else(|| e.get("responseNodes")),
        ),
    ] {
        if text.is_none() && nodes.is_none() {
            continue;
        }
        w.slot(i * 10_000);
        let content = if let Some(text) = text {
            parts(text, w.provider)
        } else {
            node_parts(nodes.unwrap_or(&Value::Null), w.provider)
        };
        let mut v = json!({"type":"message","role":role,"content":content});
        entity(&mut v, "messageId", None, w);
        // Completion time belongs to the exchange, never invented separately for each side.
        emit(spool, w, v, row)?;
        emitted = true;
    }
    if !emitted {
        emit(
            spool,
            w,
            json!({"type":"com.augmentcode.auggie/exchange"}),
            row,
        )?;
    }
    Ok(())
}

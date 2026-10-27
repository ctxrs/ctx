use super::*;
use rusqlite::OptionalExtension;

pub(super) fn response(spool: &Spool, n: &Value) -> Result<Option<String>> {
    if let Some(id) = n["llm_response_id"].as_str() {
        return Ok(Some(id.into()));
    }
    if let Some(id) = n["tool_call_id"].as_str() {
        return Ok(spool
            .db
            .query_row(
                "SELECT response FROM batch_calls WHERE id=?1 GROUP BY id HAVING count(*)=1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten());
    }
    Ok(None)
}
fn attachment(spool: &Spool, mut parent: Option<String>) -> Result<(Option<String>, bool)> {
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = parent {
        if id == "__root__" {
            return Ok((None, true));
        }
        if !seen.insert(id.clone()) {
            return Ok((None, false));
        }
        if let Some(output) = spool.output(&id)? {
            return Ok((Some(output), true));
        }
        let next: Option<Option<String>> = spool
            .db
            .query_row(
                "SELECT parent FROM batch_parents WHERE event=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        match next {
            Some(p) => parent = p,
            None => return Ok((None, false)),
        }
    }
    Ok((None, true))
}
pub(super) fn structure(spool: &Spool, w: &Writer, v: &mut Value, row: &Value) -> Result<()> {
    let n = &row["value"];
    let Some(event) = n["id"].as_str() else {
        return Ok(());
    };
    if spool.output(event)?.is_some() {
        return Ok(());
    }
    let parent: Option<String> = spool
        .db
        .query_row(
            "SELECT parent FROM batch_parents WHERE event=?1",
            [event],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let (p, known) = attachment(spool, parent)?;
    if !known {
        v["branchesFrom"] = json!({"state":"unknown"});
        return Ok(());
    }
    if p == w.tip {
        return Ok(());
    }
    if p.is_none() {
        v["branchesFrom"] = json!({"record":null});
        return Ok(());
    }
    let rk = response(spool, n)?;
    let parent_rk: Option<String> = spool
        .db
        .query_row(
            "SELECT response FROM batch_context WHERE id=?1",
            [p.as_deref()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let injected = matches!(v["author"].as_str(), Some("harness" | "hook"));
    let mut cursor = w.tip.clone();
    let mut mechanical = true;
    while cursor != p {
        let Some(ref id) = cursor else {
            mechanical = false;
            break;
        };
        let fact: Option<(Option<String>, Option<String>, bool)> = spool
            .db
            .query_row(
                "SELECT parent,response,mechanical FROM batch_context WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((prev, r, automatic)) = fact else {
            mechanical = false;
            break;
        };
        if !(injected
            || automatic
            || r.as_ref()
                .is_some_and(|r| Some(r) == rk.as_ref() || Some(r) == parent_rk.as_ref()))
        {
            mechanical = false;
        }
        cursor = prev;
    }
    if !mechanical {
        v["branchesFrom"] = json!({"record":p});
    }
    Ok(())
}
pub(super) fn register(
    spool: &Spool,
    w: &Writer,
    v: &Value,
    row: &Value,
    id: &str,
    previous: Option<String>,
) -> Result<()> {
    let n = &row["value"];
    let Some(event) = n["id"].as_str() else {
        return Ok(());
    };
    let parent = if v.get("branchesFrom").is_some() {
        v.pointer("/branchesFrom/record")
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        previous
    };
    let automatic = matches!(v["author"].as_str(), Some("harness" | "hook"))
        || matches!(v["status"].as_str(), Some("error" | "interrupted"));
    spool.db.execute(
        "INSERT INTO batch_context VALUES(?1,?2,?3,?4,?5,?6)",
        params![event, id, parent, response(spool, n)?, automatic, w.count],
    )?;
    spool.set_output(event, id)?;
    Ok(())
}
fn event_content(n: &Value, p: Provider) -> Vec<Value> {
    let mut content = parts(&n["content"], p);
    if let Some(s) = n["reasoning_content"].as_str() {
        content.insert(0, json!({"type":"reasoning","visibility":"full","text":s}));
    }
    for b in n["thinking_blocks"].as_array().into_iter().flatten() {
        content.extend(parts(b, p));
    }
    if let Some(r) = n.get("responses_reasoning_item").filter(|v| !v.is_null()) {
        if let Some(encrypted) = r["encrypted_content"].as_str() {
            content.push(json!({"type":"reasoning","visibility":"encrypted","opaque":{"format":"openai.responses.reasoning","data":encrypted}}));
        }
        for s in r["summary"].as_array().into_iter().flatten() {
            if let Some(text) = s.as_str().or_else(|| s["text"].as_str()) {
                content.push(json!({"type":"reasoning","visibility":"summary","text":text}));
            }
        }
    }
    content
}
pub(super) fn convert(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    if row["kind"] == "state" {
        return state(spool, w, row);
    }
    let n = &row["value"];
    let kind = string(n, &["kind", "type"]).unwrap_or("");
    w.slot(0);
    match kind {
        "MessageEvent" => {
            let m = &n["llm_message"];
            let mut content = event_content(m, w.provider);
            content.extend(parts(&n["extended_content"], w.provider));
            let mut v = json!({"type":"message","content":content});
            entity(&mut v, "messageId", n["id"].as_str(), w);
            if let Some(role) = m["role"]
                .as_str()
                .filter(|r| matches!(*r, "user" | "assistant" | "system" | "developer"))
            {
                v["role"] = json!(role);
            } else {
                v["capture"] = json!({"role":{"state":"absent"}});
            }
            if let Some(sender) = n["sender"].as_str() {
                v["from"] = json!({"address":sender});
            }
            emit(spool, w, v, row)?;
        }
        "ActionEvent" => {
            let mut content = event_content(n, w.provider);
            content.extend(parts(&n["thought"], w.provider));
            if !content.is_empty() {
                let mut v =
                    json!({"type":"message","role":"assistant","content":content,"author":"model"});
                entity(&mut v, "messageId", n["id"].as_str(), w);
                emit(spool, w, v, row)?;
            }
            w.slot(10_000);
            let mut c = n["tool_call"].clone();
            if !c.is_object() {
                c = json!({});
            }
            if c.get("id").is_none() {
                c["id"] = n["tool_call_id"].clone();
            }
            if c.get("name").is_none() && c.get("function").is_none() {
                c["name"] = n["tool_name"].clone();
            }
            messages::call(spool, w, row, &c, true)?;
        }
        "ObservationEvent" => {
            let obs = &n["observation"];
            // to_llm_content is tool-specific; a structured observation alone is not rendered output.
            let content = obs.get("to_llm_content").map(|v| {
                let mut c = parts(v, w.provider);
                c.extend(parts(&n["extended_content"], w.provider));
                c
            });
            messages::result(
                spool,
                w,
                row,
                n["tool_call_id"].as_str(),
                messages::status(obs),
                content,
                Some(obs),
            )?;
        }
        "UserRejectObservation" => {
            let c = n["rejection_reason"]
                .as_str()
                .map(|s| vec![json!({"type":"text","text":format!("Action rejected: {s}")})]);
            messages::result(
                spool,
                w,
                row,
                n["tool_call_id"].as_str(),
                "denied",
                c,
                Some(n),
            )?;
        }
        "AgentErrorEvent" => {
            messages::result(
                spool,
                w,
                row,
                n["tool_call_id"].as_str(),
                "failed",
                n["error"].as_str().map(|s| parts(&json!(s), w.provider)),
                Some(n),
            )?;
        }
        "SystemPromptEvent" => {
            let mut content = parts(&n["system_prompt"], w.provider);
            if let Some(context) = n.get("dynamic_context").filter(|v| !v.is_null()) {
                content.extend(parts(context, w.provider));
            }
            emit(
                spool,
                w,
                json!({"type":"instructions","kind":"system_prompt","content":content}),
                row,
            )?;
            if let Some(native) = n["tools"].as_array() {
                let tools = native
                    .iter()
                    .filter_map(|t| {
                        let name = t["name"].as_str()?;
                        let mut v = json!({"name":name});
                        for (dest, key) in [
                            ("description", "description"),
                            ("inputSchema", "input_schema"),
                        ] {
                            if let Some(x) = t.get(key) {
                                v[dest] = x.clone();
                            }
                        }
                        Some(v)
                    })
                    .collect::<Vec<_>>();
                w.slot(100);
                let mut v = json!({"type":"instructions","kind":"tool_definitions","tools":tools});
                if v["tools"].as_array().map_or(0, Vec::len) != native.len() {
                    v["capture"] = json!({"tools":{"state":"truncated"}});
                }
                emit(spool, w, v, row)?;
            }
        }
        "HookExecutionEvent" => super::hooks::convert(spool, w, row)?,
        "Condensation" => condensation(spool, w, row)?,
        "PauseEvent" | "InterruptEvent" => {
            emit(
                spool,
                w,
                json!({"type":"dev.all-hands.openhands/control"}),
                row,
            )?;
        }
        _ => {
            emit(
                spool,
                w,
                json!({"type":"dev.all-hands.openhands/event"}),
                row,
            )?;
        }
    }
    if let Some(id) = n["llm_response_id"].as_str() {
        if spool.entity(&format!("inference:{id}"))?.is_none() {
            w.slot(600_000);
            let record = emit(
                spool,
                w,
                json!({"type":"inference","inferenceId":id,"idSource":"native"}),
                row,
            )?;
            spool.save(&format!("inference:{id}"), &json!(record))?;
        }
    }
    Ok(())
}
fn condensation(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    let n = &row["value"];
    let mut ids = Vec::new();
    let mut complete = true;
    for event in n["forgotten_event_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let mut stmt = spool
            .db
            .prepare("SELECT id FROM batch_context WHERE event=?1 ORDER BY pos,id")?;
        let target = if let Some(original) = event.strip_suffix("-summary") {
            if spool.entity(&format!("condensation:{original}"))?.is_some() {
                original
            } else {
                event
            }
        } else {
            event
        };
        let records = stmt
            .query_map([target], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if records.is_empty() && spool.entity(&format!("system:{event}"))?.is_none() {
            complete = false;
        }
        ids.extend(records);
    }
    let summary = n["summary"]
        .as_str()
        .filter(|_| n["summary_offset"].as_u64().is_some());
    let mut v = json!({"type":"compaction","kind":if summary.is_some(){"summary"}else{"prune"},"replaces":if complete{json!({"ids":ids})}else{json!({"state":"unknown"})}});
    entity(&mut v, "compactionId", n["id"].as_str(), w);
    if let Some(summary) = summary {
        v["summary"] = json!([{"type":"text","text":summary}]);
    }
    // Compute the native view on this event's ancestor path, then apply its offset.
    if let Some(offset) = n["summary_offset"].as_u64().filter(|_| summary.is_some()) {
        let path = view(spool, n["id"].as_str().unwrap_or(""))?;
        let forgotten = n["forgotten_event_ids"].as_array();
        let kept = path
            .into_iter()
            .filter(|(event, _)| {
                !forgotten.is_some_and(|a| a.iter().any(|v| v.as_str() == Some(event)))
            })
            .collect::<Vec<_>>();
        let after = kept
            .iter()
            .take((offset as usize).min(kept.len()))
            .rev()
            .find_map(|(_, id)| id.as_ref());
        if let Some(after) = after {
            v["insertAt"] = json!({"afterId":after});
        } else if let Some((event, Some(id))) = kept
            .iter()
            .skip((offset as usize).min(kept.len()))
            .find(|(_, id)| id.is_some())
        {
            let first: Option<String> = spool
                .db
                .query_row(
                    "SELECT id FROM batch_context WHERE event=?1 ORDER BY pos LIMIT 1",
                    [event],
                    |r| r.get(0),
                )
                .optional()?;
            v["insertAt"] = json!({"beforeId":first.as_ref().unwrap_or(id)});
        }
    }
    emit(spool, w, v, row)?;
    Ok(())
}
fn view(spool: &Spool, event: &str) -> Result<Vec<(String, Option<String>)>> {
    let mut events = Vec::new();
    let mut cursor: Option<String> = spool
        .db
        .query_row(
            "SELECT parent FROM batch_parents WHERE event=?1",
            [event],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            break;
        }
        cursor = spool
            .db
            .query_row(
                "SELECT parent FROM batch_parents WHERE event=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        events.push(id);
    }
    events.reverse();
    let mut view: Vec<(String, Option<String>)> = Vec::new();
    for event in events {
        let pos: Option<u64> = spool
            .db
            .query_row(
                "SELECT pos FROM rows WHERE native_id=?1 AND kind='event'",
                [&event],
                |r| r.get(0),
            )
            .optional()?;
        // Stored condensation metadata is enough to update the view without retaining event payloads.
        if let Some(c) = spool.entity(&format!("condensation:{event}"))? {
            view.retain(|(id, _)| {
                !c["forgotten_event_ids"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(id.as_str())))
            });
            if let (Some(offset), Some(id)) = (c["summary_offset"].as_u64(), spool.output(&event)?)
            {
                if c["summary"].is_string() {
                    let at = (offset as usize).min(view.len());
                    view.insert(at, (format!("{event}-summary"), Some(id)));
                }
            }
        } else if pos.is_some() {
            if let Some(id) = spool.output(&event)? {
                view.push((event, Some(id)));
            } else if spool.entity(&format!("system:{event}"))?.is_some() {
                view.push((event, None));
            }
        }
    }
    Ok(view)
}
fn state(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    let n = &row["value"];
    let mut row = row.clone();
    if let Some(o) = row["value"].as_object_mut() {
        o.remove("secret_registry");
    }
    w.slot(800_000);
    let mut v = json!({"type":"session_info"});
    if let Some(leaf) = n["leaf_event_id"].as_str() {
        let (id, known) = attachment(spool, Some(leaf.into()))?;
        if known {
            if let Some(id) = id {
                v["heads"] = json!([id]);
            }
        }
    }
    emit(spool, w, v, &row)?;
    w.slot(810_000);
    let mut config = json!({"type":"config_change","snapshot":true});
    if let Some(policy) = n.get("confirmation_policy") {
        config["options"] = json!({"confirmation_policy":policy});
    }
    if let Some(model) = n.pointer("/agent/llm/model").and_then(Value::as_str) {
        config["model"] = json!({"id":model});
    }
    if config.get("options").is_some() || config.get("model").is_some() {
        emit(spool, w, config, &row)?;
    }
    let status = match n["execution_status"].as_str() {
        Some("idle" | "paused" | "finished") => Some("idle"),
        Some("running") => Some("running"),
        Some("waiting_for_confirmation") => Some("requires_action"),
        Some("error" | "stuck") => Some("error"),
        _ => None,
    };
    if let Some(status) = status {
        w.slot(820_000);
        emit(spool, w, json!({"type":"state","state":status}), &row)?;
    }
    super::metrics::state(spool, w, &row)?;
    Ok(())
}

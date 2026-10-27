use crate::{
    content,
    records::{copy, meta, path, session_ref, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};

pub(crate) enum Arguments {
    Json(Value),
    Text(String),
}

pub(crate) fn call(
    writer: &mut Writer,
    spool: &Spool,
    source: &Value,
    id: &str,
    name: Option<&str>,
    arguments: Arguments,
    requested: &str,
) -> Result<Value> {
    let (input, text) = match arguments {
        Arguments::Json(v) if !ctx_history_asr_core::numbers::safe(&v) => {
            (json!(ctx_history_asr_core::numbers::text(&v)), true)
        }
        Arguments::Json(v) => (v, false),
        Arguments::Text(s) => (json!(s), true),
    };
    let mut v =
        json!({"type":"tool_call","callId":id,"callIdSource":"native","requestedBy":requested});
    if let Some(name) = name {
        v["name"] = json!(name);
    } else {
        v["capture"] = json!({"name":{"state":"absent"}});
    }
    if !input.is_null() {
        v[if text { "inputText" } else { "input" }] = input.clone();
    }
    if let Some(name) = name {
        if name.starts_with("mcp__") {
            v["provider"] = json!({"kind":"mcp","identity":"unknown"});
        }
        if writer.provider == Provider::Claude {
            v["kind"] = json!(match name {
                "Bash" | "PowerShell" => "execute",
                "Edit" | "Write" | "NotebookEdit" => "edit",
                "Read" => "read",
                "Glob" | "Grep" | "ToolSearch" => "search",
                "WebFetch" | "WebSearch" => "fetch",
                _ => "other",
            });
        }
    }
    copy(&mut v, "parentCallId", source, "parent_call_id");
    if let Some(cell) = source.get("cell_id").or_else(|| source.get("cellId")) {
        meta(&mut v, writer.provider, "cellId", cell);
    }
    if let Some(value) = input.get("dangerouslyDisableSandbox") {
        meta(&mut v, writer.provider, "dangerouslyDisableSandbox", value);
    }
    if writer.provider == Provider::Claude {
        let mut wire = json!({});
        for key in ["wireToolInputs", "wireIngestContext"] {
            if let Some(value) = source.get(key).and_then(|map| map.get(id)) {
                wire[key] = value.clone();
            }
        }
        if wire.as_object().is_some_and(|o| !o.is_empty()) {
            meta(&mut v, writer.provider, "wireToolInputs", &wire);
        }
    }
    spool.save(
        &format!("call:{id}"),
        &json!({"name":name,"input":input,"source":source.get("message").and_then(|m|m.get("id"))}),
    )?;
    Ok(v)
}

pub(crate) fn call_audit(
    writer: &mut Writer,
    spool: &Spool,
    source: &Value,
    id: &str,
    name: &str,
    input: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let name = name.strip_prefix("functions.").unwrap_or(name);
    if matches!(name, "AskUserQuestion" | "request_user_input") {
        question(writer, spool, source, id, Some(id), input, None)?;
    }
    if matches!(name, "TodoWrite" | "update_plan") {
        let items = input
            .get("todos")
            .or_else(|| input.get("plan"))
            .and_then(Value::as_array);
        if let Some(items) = items {
            let entries:Vec<Value>=items.iter().filter_map(|item|{
                let text=item.get("content").or_else(||item.get("step"))?.as_str()?;
                Some(json!({"content":text,"status":item.get("status").cloned().unwrap_or(json!("_rs.ctx/unknown"))}))
            }).collect();
            writer.revise(spool,"plan:items",json!({"type":"plan","planId":"items","idSource":"derived","planType":"items","callId":id,"entries":entries}),source)?;
        }
    }
    if name == "ExitPlanMode" {
        if let Some(plan) = input.get("plan") {
            let plan_id = input
                .get("planFilePath")
                .and_then(Value::as_str)
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or(id);
            let mut v = json!({"type":"plan","planType":"document","planId":plan_id,"callId":id,"content":content::parts(plan,writer.provider)});
            if plan_id == id {
                v["idSource"] = json!("derived");
            }
            if let Some(p) = input["planFilePath"].as_str() {
                v["source"] = path(p, cwd);
            }
            writer.revise(spool, &format!("plan:{plan_id}"), v, source)?;
        }
    }
    Ok(())
}

pub(crate) fn question(
    writer: &mut Writer,
    spool: &Spool,
    source: &Value,
    id: &str,
    call_id: Option<&str>,
    input: &Value,
    output: Option<&Value>,
) -> Result<()> {
    let Some(questions) = input.get("questions").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut normalized = Vec::new();
    let mut answers = Vec::new();
    let mut secret = false;
    for (n, q) in questions.iter().enumerate() {
        let qid = q["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| n.to_string());
        let Some(text) = q
            .get("question")
            .or_else(|| q.get("text"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let mut v = json!({"id":qid,"text":text});
        for k in ["header", "multiSelect", "secret", "allowOther"] {
            copy(&mut v, k, q, k);
        }
        secret |= q["secret"] == true || q["isSecret"] == true;
        copy(&mut v, "secret", q, "isSecret");
        copy(&mut v, "allowOther", q, "isOther");
        if let Some(options) = q["options"].as_array() {
            v["options"] = json!(options
                .iter()
                .filter(|o| o["label"].is_string())
                .map(|o| {
                    let mut r = json!({"label":o["label"]});
                    for k in ["value", "description"] {
                        copy(&mut r, k, o, k);
                    }
                    r
                })
                .collect::<Vec<_>>());
        }
        if q["secret"] != true && q["isSecret"] != true {
            if let Some(answer) = output
                .and_then(|o| o.get("answers"))
                .and_then(|a| a.get(&qid).or_else(|| a.get(text)))
            {
                let values = answer.get("answers").unwrap_or(answer);
                let mut a = json!({"questionId":qid});
                let labels: Vec<&str> = q["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o["label"].as_str())
                    .collect();
                if let Some(s) = values.as_str() {
                    if labels.contains(&s) {
                        a["selected"] = json!([s]);
                    } else {
                        a["text"] = json!(s);
                    }
                } else if let Some(values) = values.as_array() {
                    let selected: Vec<&str> = values
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|s| labels.contains(s))
                        .collect();
                    let free: Vec<&str> = values
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|s| !labels.contains(s))
                        .collect();
                    if !selected.is_empty() {
                        a["selected"] = json!(selected);
                    }
                    if free.len() == 1 {
                        a["text"] = json!(free[0]);
                    } else if !free.is_empty() {
                        meta(&mut a, writer.provider, "answerTexts", &json!(free));
                    }
                }
                answers.push(a);
            }
        }
        normalized.push(v);
    }
    let mut v = json!({"type":"input_request","requestId":id,"idSource":"derived","mode":"questions","questions":normalized});
    if let Some(call_id) = call_id {
        v["callId"] = json!(call_id);
    }
    copy(&mut v, "blocking", input, "blocking");
    copy(&mut v, "blocking", input, "isBlocking");
    if let Some(output) = output {
        if output.get("afkTimeoutMs").is_some() {
            v["status"] = json!("timed_out");
        } else if output.get("answers").is_some() || output.get("response").is_some() {
            v["status"] = json!("answered");
        }
        if !answers.is_empty() {
            v["answers"] = json!(answers);
        }
        if !secret {
            copy(&mut v, "response", output, "response");
        }
        for k in ["afkTimeoutMs", "annotations"] {
            if let Some(value) = output.get(k) {
                meta(&mut v, writer.provider, k, value);
            }
        }
    }
    if secret {
        spool.save(&format!("secret:{id}"), &json!(true))?;
    }
    writer.revise(spool, &format!("question:{id}"), v, source)?;
    Ok(())
}

pub(crate) fn result_audit(
    writer: &mut Writer,
    spool: &Spool,
    source: &Value,
    id: &str,
    output: &Value,
    status: &str,
    cwd: Option<&str>,
) -> Result<()> {
    if spool.ambiguous_call(id)? {
        return Ok(());
    }
    let Some(call) = spool.entity(&format!("call:{id}"))? else {
        return Ok(());
    };
    let name = call["name"].as_str().unwrap_or("");
    let input = &call["input"];
    let name = name.strip_prefix("functions.").unwrap_or(name);
    if matches!(name, "AskUserQuestion" | "request_user_input") {
        question(writer, spool, source, id, Some(id), input, Some(output))?;
    }
    if matches!(name, "Edit" | "Write" | "NotebookEdit") {
        if let Some(file) = output
            .get("filePath")
            .or_else(|| input.get("file_path"))
            .or_else(|| input.get("notebook_path"))
            .and_then(Value::as_str)
        {
            let mut change = path(file, cwd);
            change["op"] = json!(if output["type"] == "create" {
                "add"
            } else if name == "Write" && output["type"] != "update" {
                "unknown"
            } else {
                "modify"
            });
            let mut v = json!({"type":"file_change","cause":{"callId":id},"status":match status {"completed"=>"applied","failed"=>"failed","denied"=>"skipped",_=>"unknown"},"precision":"unknown","changes":[change]});
            if name == "Write" && input["content"].is_string() {
                v["precision"] = json!("exact");
                v["changes"][0]["after"] = json!({"text":input["content"]});
            } else if name == "Edit" {
                v["precision"] = json!("snippet");
                meta(
                    &mut v,
                    writer.provider,
                    "edit",
                    &json!({"old_string":input["old_string"],"new_string":input["new_string"]}),
                );
            }
            if let Some(patch) = crate::claude::coding::hunks(&output["structuredPatch"]) {
                v["precision"] = json!("hunks");
                v["patch"] = json!({"format":"unified","text":patch});
            }
            if output["userModified"] == true {
                v["changes"][0].as_object_mut().map(|o| o.remove("after"));
                if v.get("patch").is_none() {
                    v["precision"] = json!("unknown");
                }
            }
            copy(&mut v, "modifiedByUser", output, "userModified");
            writer.emit(v, source)?;
            if output["memdirStamped"] == true {
                writer.emit(json!({"type":"memory_change","store":"auto_memory","scope":"project","op":"update","target":{"path":path(file,cwd)},"cause":{"callId":id},"status":if status=="completed" {"applied"} else {"rejected"}}),source)?;
            }
        }
    }
    if matches!(name, "Agent" | "Task" | "spawn_agent") {
        let child = output
            .get("agentId")
            .or_else(|| output.get("agent_id"))
            .and_then(Value::as_str);
        let mut v = json!({"type":"subagent","callId":id});
        if let Some(child) = child {
            v["childSession"] = json!(session_ref(writer.provider, child));
        } else {
            v["capture"] = json!({"childSession":{"state":"absent"}});
        }
        for (dest, key) in [
            ("async", "run_in_background"),
            ("description", "description"),
            ("role", "subagent_type"),
        ] {
            copy(&mut v, dest, input, key);
        }
        if let Some(model) = input["model"].as_str() {
            v["model"] = json!({"id":model});
        }
        let mut stats = json!({});
        for k in ["totalDurationMs", "totalTokens", "toolStats"] {
            copy(&mut stats, k, output, k);
        }
        if stats.as_object().is_some_and(|o| !o.is_empty()) {
            meta(&mut v, writer.provider, "agentStats", &stats);
        }
        let key = format!("subagent:{}", child.unwrap_or(id));
        writer.revise(spool, &key, v, source)?;
    }
    if matches!(name, "Bash" | "PowerShell" | "Monitor") {
        if let Some(terminal) = output
            .get("backgroundTaskId")
            .or_else(|| output.get("taskId"))
            .and_then(Value::as_str)
        {
            let mut v = json!({"type":"terminal","terminalId":terminal,"callId":id,"kind":if name=="Monitor" {"monitor"}else{"shell"}});
            copy(&mut v, "command", input, "command");
            copy(&mut v, "description", input, "description");
            writer.revise(spool, &format!("terminal:{terminal}"), v, source)?;
        }
    }
    if name == "TaskStop" {
        if let Some(task) = input["task_id"].as_str() {
            for family in ["terminal", "subagent"] {
                let key = format!("{family}:{task}");
                if let Some(mut v) = spool.entity(&key)? {
                    v.as_object_mut().map(|o| o.remove("_record"));
                    v["status"] = json!("stopped");
                    writer.revise(spool, &key, v, source)?;
                }
            }
        }
    }
    Ok(())
}

/// UI execution details and model-facing output revise one result without erasing either view.
pub(crate) fn result(
    writer: &mut Writer,
    spool: &Spool,
    id: &str,
    mut value: Value,
    source: &Value,
) -> Result<()> {
    if spool.ambiguous_call(id)? {
        value["linkage"] = json!("ambiguous");
        value.as_object_mut().unwrap().remove("callId");
        meta(&mut value, writer.provider, "unpairedCallId", &json!(id));
        writer.emit(value, source)?;
        return Ok(());
    }
    let key = format!("result:{id}");
    if let Some(mut previous) = spool.entity(&key)? {
        if let Some(fields) = previous.as_object_mut() {
            fields.remove("_record");
            fields.remove("supersedes");
            if value
                .pointer("/capture/content/state")
                .and_then(Value::as_str)
                == Some("absent")
                && fields.contains_key("content")
            {
                value
                    .get_mut("capture")
                    .and_then(Value::as_object_mut)
                    .map(|o| o.remove("content"));
            }
            if value["status"] == "unknown" && previous["status"] != "unknown" {
                value.as_object_mut().map(|o| o.remove("status"));
            }
            // A newly observed field replaces its earlier capture state too.
            // In particular, a UI-only result can precede model-facing content.
            if let (Some(capture), Some(incoming)) = (
                previous.get_mut("capture").and_then(Value::as_object_mut),
                value.as_object(),
            ) {
                for key in incoming.keys() {
                    if value.get("capture").and_then(|c| c.get(key)).is_none() {
                        capture.remove(key);
                    }
                }
            }
            if let (Some(to), Some(from)) = (previous.as_object_mut(), value.as_object()) {
                for (k, v) in from {
                    if k == "capture" || k == "_meta" {
                        let dest = to.entry(k.clone()).or_insert(json!({}));
                        if let (Some(dest), Some(src)) = (dest.as_object_mut(), v.as_object()) {
                            dest.extend(src.clone());
                        }
                    } else {
                        to.insert(k.clone(), v.clone());
                    }
                }
            }
            value = previous;
        }
    }
    writer.revise(spool, &key, value, source)?;
    Ok(())
}

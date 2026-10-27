use crate::{
    content,
    records::{copy, meta, path, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
pub(super) fn attachment(
    spool: &Spool,
    writer: &mut Writer,
    v: &Value,
    cwd: Option<&str>,
) -> Result<bool> {
    let a = &v["attachment"];
    match a["type"].as_str() {
        Some("environment") => {
            let snapshot = &a["snapshot"];
            let mut state = json!({"type":"workspace_state"});
            if let Some(directory) = snapshot["workingDirectory"].as_str() {
                state["cwd"] = path(directory, cwd);
            }
            if let Some(linked) = snapshot["isWorktree"].as_bool() {
                state["worktree"] = json!({"linked":linked});
            }
            if state.get("cwd").is_some() {
                writer.emit(state, v)?;
            }
            let mut environment = json!({});
            for (to, from) in [("os", "platform"), ("shell", "shell")] {
                copy(&mut environment, to, snapshot, from);
            }
            if environment.as_object().is_some_and(|o| !o.is_empty()) {
                writer.emit(json!({"type":"declaration","environment":environment}), v)?;
            }
            for (i, dir) in snapshot["additionalWorkingDirectories"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .enumerate()
            {
                let key = format!("root:{dir}");
                if spool.entity(&key)?.is_none() {
                    let id = format!("extra-{}-{i}", writer.pos);
                    writer.emit(
                        json!({"type":"declaration","roots":[{"id":id,"path":dir}]}),
                        v,
                    )?;
                    spool.save(&key, &json!(id))?;
                }
            }
        }
        Some("deferred_tools_delta") => {
            let mut r = json!({"type":"instructions","kind":"tool_definitions"});
            copy(&mut r, "removed", a, "removedNames");
            if let Some(lines) = a["addedLines"].as_array() {
                r["content"] = json!(lines
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| json!({"type":"text","text":s}))
                    .collect::<Vec<_>>());
            } else {
                r["capture"] = json!({"content":{"state":"absent"}});
            }
            let mut delta = json!({});
            for key in [
                "wireHiddenNames",
                "surfacedNames",
                "retractedTools",
                "readdedNames",
            ] {
                copy(&mut delta, key, a, key);
            }
            meta(&mut r, Provider::Claude, "toolsDelta", &delta);
            writer.emit(r, v)?;
            for (field, op) in [
                ("failedMcpServers", "fail"),
                ("needsAuthMcpServers", "needs_auth"),
            ] {
                for server in a[field].as_array().into_iter().flatten() {
                    if let Some(name) = server.as_str().or_else(|| server["name"].as_str()) {
                        writer.emit(json!({"type":"config_change","capability":{"kind":"mcp_server","op":op,"name":name}}),v)?;
                    }
                }
            }
        }
        Some("goal_status") => {
            let mut r = json!({"type":"plan","planId":"goal","idSource":"derived","planType":"goal","content":content::parts(&a["condition"],Provider::Claude)});
            if let Some(met) = a["met"].as_bool() {
                r["status"] = json!(if met { "complete" } else { "active" });
            }
            meta(
                &mut r,
                Provider::Claude,
                "goalStatus",
                &json!({"iterations":a["iterations"],"sentinel":a["sentinel"],"met":a["met"]}),
            );
            writer.revise(spool, "plan:goal", r, v)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

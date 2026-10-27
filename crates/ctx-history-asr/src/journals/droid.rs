use super::*;
pub(super) fn convert(
    s: &mut Spool,
    w: &mut Writer,
    _pos: u64,
    v: &Value,
    _cwd: Option<&str>,
) -> Result<()> {
    match v["type"].as_str().unwrap_or("unknown") {
        "message" => {
            let body = v
                .get("content")
                .or_else(|| v.pointer("/message/content"))
                .unwrap_or(&Value::Null);
            let role = v["role"]
                .as_str()
                .or_else(|| v.pointer("/message/role").and_then(Value::as_str));
            if let Some(role) =
                role.filter(|r| matches!(*r, "user" | "assistant" | "system" | "developer"))
            {
                blocks::message(
                    s,
                    w,
                    v,
                    body,
                    role,
                    v["id"].as_str().or_else(|| v["uuid"].as_str()),
                )?;
            } else {
                w.vendor(v, "message")?;
            }
        }
        "compaction_state" => blocks::compaction(w, v, v.get("summary"), v["id"].as_str())?,
        "todo_state" => {
            if let Some(items) = v["items"].as_array() {
                let entries:Vec<_>=items.iter().filter_map(|i|Some(json!({"content":i["content"].as_str()?,"status":match i["status"].as_str(){Some("pending")=>"pending",Some("in_progress")=>"in_progress",Some("completed")=>"completed",_=>"_ai.factory.droid/unknown"}}))).collect();
                let mut r = json!({"type":"plan","planType":"items","planId":"todos","idSource":"derived","entries":entries});
                retain(&mut r, w.provider, v, &[]);
                w.revise(s, "droid:todos", r, v)?;
            } else {
                w.vendor(v, "todo_state")?;
            }
        }
        kind => w.vendor(v, kind)?,
    }
    Ok(())
}

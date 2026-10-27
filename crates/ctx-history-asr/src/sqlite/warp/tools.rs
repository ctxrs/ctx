use super::*;
use ctx_history_providers_sqlite_selected::warp_wire::{
    is_warp_tool_arm, is_warp_tool_result_arm, warp_tool_name,
};
pub(super) fn call(
    spool: &Spool,
    writer: &mut Writer,
    row: &Value,
    fields: &Fields<'_>,
) -> Result<Value> {
    let id = fields.text(1)?.filter(|s| !s.is_empty());
    let mut out = json!({"type":"tool_call","callId":id.map(str::to_owned).unwrap_or_else(||writer.id()),"callIdSource":if id.is_some(){"native"}else{"derived"},"requestedBy":"model"});
    if let Some((kind, payload)) = fields.oneof(is_warp_tool_arm) {
        let f = Fields::parse(&payload)?;
        if kind == 12 {
            if let Some(name) = f.text(1)? {
                out["name"] = json!(name);
            } else {
                out["capture"] = json!({"name":{"state":"absent"}});
            }
            let server = f.text(3)?.filter(|s| !s.is_empty());
            if let (Some(server), Some(name)) = (server, f.text(1)?.filter(|s| !s.is_empty())) {
                out["provider"] =
                    json!({"kind":"mcp","identity":"exact","server":server,"tool":name});
            } else {
                out["provider"] = json!({"kind":"mcp","identity":"unknown"});
            }
            if let Some(args) = f.merged(2) {
                let value = proto::object(&args, 0)?;
                if safe(&value) {
                    out["input"] = value;
                } else {
                    out["inputText"] = json!(value.to_string());
                }
            }
        } else {
            let name = warp_tool_name(kind);
            if name != "unknown" {
                out["name"] = json!(name);
            } else {
                out["capture"] = json!({"name":{"state":"absent"}});
            }
            out["provider"] = json!({"kind":"builtin","identity":"exact"});
            if kind == 2 {
                if let Some(command) = f.text(1)? {
                    out["input"] = json!({"command":command});
                    out["kind"] = json!("execute");
                }
            }
            if out.get("input").is_none() {
                if !out["capture"].is_object() {
                    out["capture"] = json!({});
                }
                out["capture"]["input"] = json!({"state":"omitted","reason":"unsupported"});
            }
        }
        meta(
            &mut out,
            P,
            "inputProtobuf",
            &blob(spool, &payload, "application/x-protobuf")?,
        );
    } else {
        out["capture"] = json!({"name":{"state":"absent"},"input":{"state":"absent"}});
    }
    meta(&mut out, P, "taskId", &row["task_id"]);
    Ok(out)
}
pub(super) fn result(spool: &Spool, fields: &Fields<'_>) -> Result<Value> {
    let mut out =
        json!({"type":"tool_result","status":"unknown","capture":{"content":{"state":"absent"}}});
    link(spool, &mut out, fields.text(1)?, P)?;
    if let Some(context) = fields.merged(11) {
        meta(
            &mut out,
            P,
            "inputContext",
            &blob(spool, &context, "application/x-protobuf")?,
        );
    }
    if let Some((kind, payload)) = fields.oneof(is_warp_tool_result_arm) {
        let f = Fields::parse(&payload)?;
        meta(
            &mut out,
            P,
            "resultProtobuf",
            &blob(spool, &payload, "application/x-protobuf")?,
        );
        if kind == 14 {
            out["status"] = json!("cancelled");
        } else if kind == 16 {
            if let Some((variant, data)) = f.oneof(|n| n == 1 || n == 2) {
                let r = Fields::parse(&data)?;
                if variant == 2 {
                    out["status"] = json!("failed");
                    if let Some(text) = r.text(1)? {
                        out["error"] = json!({"message":text});
                    }
                } else {
                    out["status"] = json!("completed");
                    let mut parts = Vec::new();
                    for entry in r.all(1) {
                        let entry = Fields::parse(entry)?;
                        if let Some((kind, part)) = entry.oneof(|n| (1..=3).contains(&n)) {
                            let p = Fields::parse(&part)?;
                            match kind{
                        1=>if let Some(text)=p.text(1)?{parts.push(json!({"type":"text","text":text}));},
                        2=>parts.push(binary(spool,p.text(2)?.unwrap_or("application/octet-stream"),p.text(1)?.unwrap_or_default())?),
                        _=>parts.push(json!({"type":"opaque","format":"warp.MCPResourceContent","blob":blob(spool,&part,"application/x-protobuf")?})),
                    }
                        }
                    }
                    // Stored MCP blocks are the raw result. Server-side rendering is not in this cache.
                    out["raw"] = json!({"content":parts});
                }
            }
        } else if kind == 2 {
            out["exec"] = json!({});
            if let Some(command) = f.text(3)? {
                out["exec"]["command"] = json!(command);
            }
            if let Some(text) = f.text(1)? {
                out["exec"]["output"] = json!(text);
            }
            if let Some(code) = f.int(2) {
                out["exec"]["exitCode"] = json!(code as i32);
            }
            if let Some((variant, data)) = f.oneof(|n| (4..=6).contains(&n)) {
                if variant == 6 {
                    out["status"] = json!("denied");
                } else if variant == 5 {
                    out["status"] = json!("completed");
                    meta(
                        &mut out,
                        P,
                        "shellFinished",
                        &blob(spool, &data, "application/x-protobuf")?,
                    );
                }
            }
        }
    }
    Ok(out)
}

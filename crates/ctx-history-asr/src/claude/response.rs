use crate::{
    content,
    records::{extras, Writer},
    spool::Spool,
    tools, Provider, Result,
};
use ruopencode_source::{params, OptionalExtension};
use serde_json::{json, Value};
use std::io::Write;

pub(super) fn convert(spool: &mut Spool, writer: &mut Writer, pos: u64, v: &Value) -> Result<()> {
    let response = v["message"]["id"].as_str().unwrap_or_default();
    let first: u64 = spool.db.query_row(
        "SELECT min(pos) FROM rows WHERE response=?1",
        [response],
        |r| r.get(0),
    )?;
    if pos == first {
        let mut cursor = 0;
        let mut parts = Vec::new();
        let mut ids = Vec::new();
        let mut ids_size = 0;
        let mut size = 0;
        let mut last = v.clone();
        let mut last_observed = 0;
        let mut native = tempfile::NamedTempFile::new_in(&spool.bundle)?;
        native.write_all(b"[")?;
        let mut comma = false;
        while let Some(next) = spool
            .db
            .query_row(
                "SELECT pos,observed FROM rows WHERE response=?1 AND pos>?2 ORDER BY pos LIMIT 1",
                params![response, cursor],
                |r| Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?)),
            )
            .optional()?
        {
            cursor = next.0;
            let row = spool.get(next.0)?;
            if let Some(uuid) = row["uuid"].as_str() {
                ids_size += uuid.len();
                if ids_size <= spool.limit / 8 {
                    ids.push(uuid.to_owned());
                }
            }
            for block in row["message"]["content"].as_array().into_iter().flatten() {
                if comma {
                    native.write_all(b",")?;
                }
                comma = true;
                serde_json::to_writer(native.as_file_mut(), block)?;
            }
            let normalized = content::parts(&row["message"]["content"], Provider::Claude);
            size += serde_json::to_vec(&normalized)?.len();
            if size <= spool.limit {
                parts.extend(normalized);
            } else {
                parts.clear();
            }
            if next.1 > last_observed {
                last = row;
                last_observed = next.1;
            }
        }
        native.write_all(b"]")?;
        let mut m = json!({"type":"message","messageId":response,"inferenceId":response,"role":"assistant","author":"model","content":parts,"assembly":{"mode":"folded"}});
        if ids_size <= spool.limit / 8 {
            m["assembly"]["sourceIds"] = json!(ids);
        }
        if size > spool.limit {
            m["capture"] = json!({"content":{"state":"external","reason":"size_limit","blob":spool.blob(native)?}});
        }
        if v["isApiErrorMessage"] == true {
            m["status"] = json!("error");
        }
        extras(
            &mut m,
            Provider::Claude,
            v,
            &[
                "message",
                "uuid",
                "parentUuid",
                "sessionId",
                "isSidechain",
                "agentId",
                "promptId",
                "cwd",
                "version",
                "requestId",
                "isApiErrorMessage",
                "attributionAgent",
                "attributionSkill",
                "attributionPlugin",
                "attributionMcpServer",
                "attributionMcpTool",
                "thinkingDurationMs",
            ],
        );
        // Slot 0 always belongs to the folded response; growth cannot renumber its usage or tools.
        super::structure::emit(spool, writer, m, v, Some(response), true)?;
        spool.save(&format!("response:{response}"), &json!(writer.tip))?;
        writer.slot(1);
        super::usage::inference(spool, writer, &last, v, response)?;
    }
    if let (Some(uuid), Some(rid)) = (
        v["uuid"].as_str(),
        spool
            .entity(&format!("response:{response}"))?
            .and_then(|v| v.as_str().map(str::to_owned)),
    ) {
        spool.set_output(uuid, &rid)?;
    }
    let mut first_context = pos != first;
    for (n, block) in v["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if block["type"] != "tool_use" {
            continue;
        }
        let Some(id) = block["id"].as_str() else {
            continue;
        };
        writer.slot(100 + (n as u64) * 100);
        let name = block["name"].as_str();
        let mut call = tools::call(
            writer,
            spool,
            v,
            id,
            name,
            tools::Arguments::Json(block["input"].clone()),
            "model",
        )?;
        call["inferenceId"] = json!(response);
        if name.is_some_and(|n| n.starts_with("mcp__")) {
            let next:Option<u64>=spool.db.query_row("SELECT pos FROM rows WHERE kind='assistant' AND pos>(SELECT min(position) FROM tool_results WHERE call_id=?1) ORDER BY pos LIMIT 1",[id],|r|r.get(0)).optional()?;
            if let Some(next) = next {
                let next = spool.get(next)?;
                if let (Some(server), Some(tool)) = (
                    next["attributionMcpServer"].as_str(),
                    next["attributionMcpTool"].as_str(),
                ) {
                    call["provider"] =
                        json!({"kind":"mcp","identity":"parsed","server":server,"tool":tool});
                }
            }
        }
        super::structure::emit(spool, writer, call, v, Some(response), first_context)?;
        first_context = false;
        tools::call_audit(
            writer,
            spool,
            v,
            id,
            name.unwrap_or(""),
            &block["input"],
            v["cwd"].as_str(),
        )?;
    }
    Ok(())
}

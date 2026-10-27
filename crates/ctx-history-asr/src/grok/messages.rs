use super::*;
use std::io::{Read, Seek, Write};
/// V1 has no message ID. The writer/replayer joins adjacent same-kind chunks;
/// promptIndex and promptId changes establish an additional boundary.
pub(super) fn emit(spool: &mut Spool, w: &mut Writer, v: &Value, time: &Value) -> Result<()> {
    let u = update(v);
    let kind = &u["sessionUpdate"];
    let event = jsonl::event_id(v)
        .map(str::to_owned)
        .unwrap_or_else(|| w.pos.to_string());
    let message = u["messageId"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("chunk:{event}"));
    let user = kind == "user_message_chunk";
    let thought = kind == "agent_thought_chunk";
    if user {
        if let Some(index) = u.pointer("/_meta/promptIndex").and_then(Value::as_u64) {
            spool.save(&format!("grok:prompt:{index}"), &json!(w.tip))?;
        }
    }
    let mut r = json!({"type":"message","messageId":message,"role":if user{"user"}else{"assistant"},"content":[]});
    if !user {
        if let Some(response) = spool
            .entity("grok:response")?
            .filter(|v| v["id"].is_string())
        {
            r["inferenceId"] = response["id"].clone();
        }
    }
    if !u["messageId"].is_string() {
        r["idSource"] = json!("derived");
        if jsonl::event_id(v).is_none() {
            r["idScope"] = json!("capture");
        }
    }
    if u["content"]["type"] != "text" {
        if thought {
            r["capture"] = json!({"content":{"state":"absent","reason":"unsupported"}});
        } else {
            r["content"] = json!(content::parts(&json!([u["content"]]), P));
        }
        native(&mut r, v);
        meta(&mut r, P, "content", &u["content"]);
        w.emit(r, time)?;
        return Ok(());
    }
    let mut file = tempfile::NamedTempFile::new_in(&spool.bundle)?;
    let mut metadata = tempfile::NamedTempFile::new_in(&spool.bundle)?;
    file.write_all(if thought {
        b"[{\"type\":\"reasoning\",\"text\":\""
    } else {
        b"[{\"type\":\"text\",\"text\":\""
    })?;
    metadata.write_all(b"[")?;
    let mut current = v.clone();
    let mut end = w.pos;
    let mut count = 0;
    loop {
        let item = update(&current);
        let encoded = serde_json::to_vec(item["content"]["text"].as_str().unwrap_or(""))?;
        file.write_all(&encoded[1..encoded.len() - 1])?;
        if count > 0 {
            metadata.write_all(b",")?;
        }
        let mut m = json!({});
        native(&mut m, &current);
        serde_json::to_writer(metadata.as_file_mut(), &m)?;
        count += 1;
        let Some(next) = spool.next(end)? else {
            break;
        };
        let row = spool.get(next)?;
        let next_u = update(&row);
        if jsonl::session_id(P, &row) != jsonl::session_id(P, v)
            || next_u["sessionUpdate"] != *kind
            || next_u["content"]["type"] != "text"
            || next_u["messageId"] != u["messageId"]
            || next_u.pointer("/_meta/promptIndex") != u.pointer("/_meta/promptIndex")
            || row.pointer("/params/_meta/promptId") != v.pointer("/params/_meta/promptId")
        {
            break;
        }
        current = row;
        end = next;
    }
    file.write_all(b"\"")?;
    if thought {
        if let Some(next) = spool.next(end)? {
            let next = spool.get(next)?;
            let u = update(&next);
            if jsonl::session_id(P, &next) == jsonl::session_id(P, v)
                && matches!(
                    u["sessionUpdate"].as_str(),
                    Some("reasoning_completed" | "response_completed")
                )
            {
                if let Some(signature) = u["signature"].as_str() {
                    file.write_all(b",\"signature\":")?;
                    serde_json::to_writer(file.as_file_mut(), signature)?;
                }
            }
        }
    }
    file.write_all(b"}]")?;
    metadata.write_all(b"]")?;
    r["assembly"] = json!({"mode":"chunks"});
    meta(&mut r, P, "chunkCount", &json!(count));
    if file.as_file().metadata()?.len() <= spool.limit as u64 {
        file.as_file_mut().rewind()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        r["content"] = serde_json::from_slice(&bytes)?;
    } else {
        r["capture"] = json!({"content":{"state":"external","blob":spool.blob(file)?}});
    }
    if metadata.as_file().metadata()?.len() <= spool.limit as u64 {
        metadata.as_file_mut().rewind()?;
        let mut bytes = Vec::new();
        metadata.read_to_end(&mut bytes)?;
        meta(&mut r, P, "chunks", &serde_json::from_slice(&bytes)?);
    } else {
        meta(&mut r, P, "chunks", &spool.blob(metadata)?);
    }
    spool.save("grok:skip", &json!(end))?;
    w.emit(r, time)?;
    Ok(())
}

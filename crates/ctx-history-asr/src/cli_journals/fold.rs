//! Fold only source-defined response parts, spilling large content to a blob.
use super::*;
use ruopencode_source::OptionalExtension;
use std::io::Write;

pub(super) fn message(s: &mut Spool, w: &Writer, group: &str) -> Result<Option<Value>> {
    let first: u64 = s.db.query_row(
        "SELECT min(position) FROM journal_parts WHERE group_id=?1",
        [group],
        |r| r.get(0),
    )?;
    if first != w.pos {
        return Ok(None);
    }
    let mut r =
        json!({"type":"message","role":"assistant","author":"model","assembly":{"mode":"folded"}});
    let mut content = Vec::new();
    let mut ids = Vec::new();
    let mut size = 0;
    let mut ids_size = 0;
    let mut cursor = 0;
    let mut count = 0;
    let mut blob = tempfile::NamedTempFile::new_in(&s.bundle)?;
    blob.write_all(b"[")?;
    let mut comma = false;
    while let Some(pos)=s.db.query_row("SELECT position FROM journal_parts WHERE group_id=?1 AND position>?2 ORDER BY position LIMIT 1",params![group,cursor],|r|r.get::<_,u64>(0)).optional()? {
        cursor=pos;count+=1;let v=s.get(pos)?;
        let (normalized,id)=if w.provider==Provider::Kimi {(parts(&json!([v["event"]["part"]]),w.provider),v["event"]["uuid"].as_str())}else{(copilot_content(&v["data"]),v["id"].as_str())};
        if let Some(id)=id {ids_size+=id.len();if ids_size<=s.limit/8 {ids.push(id.to_owned());}}
        for part in &normalized {if comma {blob.write_all(b",")?;}comma=true;serde_json::to_writer(blob.as_file_mut(),part)?;}
        size+=serde_json::to_vec(&normalized)?.len();if size<=s.limit {content.extend(normalized);}else{content.clear();}
    }
    blob.write_all(b"]")?;
    if size > s.limit {
        r["capture"] =
            json!({"content":{"state":"external","reason":"size_limit","blob":s.blob(blob)?}});
    } else {
        r["content"] = json!(content);
    }
    if ids_size <= s.limit / 8 {
        r["assembly"]["sourceIds"] = json!(ids);
    }
    meta(&mut r, w.provider, "partCount", &json!(count));
    Ok(Some(r))
}
pub(super) fn copilot_content(d: &Value) -> Vec<Value> {
    let mut content = d
        .get("content")
        .map(|v| parts(v, Provider::Copilot))
        .unwrap_or_default();
    if let Some(text) = d["reasoningText"].as_str() {
        content.insert(
            0,
            json!({"type":"reasoning","visibility":"full","text":text}),
        );
    }
    if let Some(data) = d["encryptedContent"].as_str() {
        content.push(
            json!({"type":"opaque","format":"com.github.copilot-cli.encryptedContent","data":data}),
        );
    }
    content
}

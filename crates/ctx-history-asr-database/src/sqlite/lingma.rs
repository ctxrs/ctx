use super::common::*;
use crate::{records::Writer, spool::Spool, Provider, Result};
use serde_json::{json, Value};
pub(super) fn convert(spool: &mut Spool, writer: &mut Writer, row: &Value) -> Result<()> {
    let p = Provider::Lingma;
    if let Some(prompt) = row["chat_prompt"].as_str() {
        let mut m = message(
            spool,
            row,
            p,
            None,
            Some("user"),
            vec![json!({"type":"text","text":prompt})],
        )?;
        m["messageId"] = json!(format!(
            "{}:prompt",
            row["_key"].as_str().unwrap_or_default()
        ));
        time(
            &mut m,
            row,
            "gmt_create",
            row["gmt_create"]
                .as_i64()
                .is_some_and(|n| n.unsigned_abs() > 1_000_000_000_000),
        );
        writer.emit(m, &Value::Null)?;
    }
    if let Some(summary) = row["summary"].as_str().filter(|s| !s.is_empty()) {
        writer.slot(1);
        let mut m = message(
            spool,
            row,
            p,
            None,
            Some("assistant"),
            vec![json!({"type":"text","text":summary})],
        )?;
        m["messageId"] = json!(format!(
            "{}:summary",
            row["_key"].as_str().unwrap_or_default()
        ));
        m["capture"] = json!({"content":{"state":"summarized","reason":"source_truncated"}});
        writer.emit(m, &Value::Null)?;
    }
    if let Some(error) = row["error_result"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != "{}")
    {
        writer.slot(2);
        let mut out = json!({"type":"com.aliyun.lingma/error","errorText":error});
        row_meta(spool, &mut out, p, row)?;
        writer.emit(out, &Value::Null)?;
    }
    // Even an empty prompt row is source evidence (including encrypted answer/extra columns).
    writer.slot(3);
    let mut out = json!({"type":"com.aliyun.lingma/source_context"});
    row_meta(spool, &mut out, p, row)?;
    writer.emit(out, &Value::Null)?;
    Ok(())
}

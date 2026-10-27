use crate::{records::meta, spool::Spool, Provider, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::Write;

pub(super) fn safe(v: &Value) -> bool {
    match v {
        Value::Number(n) => n
            .as_i64()
            .map(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
            .or_else(|| n.as_u64().map(|n| n <= 9_007_199_254_740_991))
            .unwrap_or_else(|| {
                n.as_f64()
                    .is_some_and(|n| n.abs() < 9_007_199_254_740_992.0)
            }),
        Value::Array(a) => a.iter().all(safe),
        Value::Object(o) => o.values().all(safe),
        _ => true,
    }
}
pub(super) fn bytes(spool: &Spool, bytes: &[u8], mime: &str) -> Result<Value> {
    let mut file = tempfile::NamedTempFile::new_in(&spool.bundle)?;
    file.write_all(bytes)?;
    let mut b = spool.blob(file)?;
    b["mimeType"] = json!(mime);
    Ok(b)
}
pub(super) fn preserve(
    spool: &Spool,
    record: &mut Value,
    provider: Provider,
    key: &str,
    value: &Value,
) -> Result<()> {
    let raw = serde_json::to_vec(value)?;
    if raw.len() > 65_536 {
        meta(
            record,
            provider,
            key,
            &bytes(spool, &raw, "application/json")?,
        );
    } else if safe(value) {
        meta(record, provider, key, value);
    } else {
        let field = format!("{}/jsonText", provider.domain());
        if record.get("_meta").is_none() {
            record["_meta"] = json!({});
        }
        if record["_meta"].get(&field).is_none() {
            record["_meta"][&field] = json!({});
        }
        record["_meta"][&field][key] = json!(String::from_utf8_lossy(&raw));
    }
    Ok(())
}
pub(super) fn content(
    spool: &Spool,
    record: &mut Value,
    source: Option<&Value>,
    provider: Provider,
) -> Result<()> {
    let Some(source) = source.filter(|v| !v.is_null()) else {
        record["content"] = json!([]);
        record["capture"]["content"] = json!({"state":"absent"});
        return Ok(());
    };
    if !safe(source) {
        record["content"] = json!([]);
        record["capture"]["content"] = json!({"state":"external","blob":bytes(spool,&serde_json::to_vec(source)?,"application/json")?});
        return Ok(());
    }
    let items: Vec<&Value> = source
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_else(|| vec![source]);
    let mut parts = Vec::new();
    for item in items {
        let mut part = if let Some(text) = item.as_str() {
            json!({"type":"text","text":text})
        } else if item["type"] == "text" && item["text"].is_string() {
            json!({"type":"text","text":item["text"]})
        } else if item["type"] == "reasoning" && item.get("visibility").is_some() {
            item.clone()
        } else if item["type"] == "thinking" && item["thinking"].is_string() {
            let mut p = json!({"type":"reasoning","visibility":"full","text":item["thinking"]});
            crate::records::copy(&mut p, "signature", item, "signature");
            p
        } else if item["type"] == "image_url" || item["type"] == "input_image" {
            image(
                spool,
                item["image_url"]
                    .as_str()
                    .or_else(|| item.pointer("/image_url/url").and_then(Value::as_str)),
                None,
            )?
            .unwrap_or_else(|| json!({"type":"data","data":item}))
        } else if (matches!(item["type"].as_str(), Some("image" | "audio"))
            && item["mimeType"].is_string())
            || (item["type"] == "resource_link"
                && item["uri"].is_string()
                && item["name"].is_string())
        {
            item.clone()
        } else {
            json!({"type":"data","schema":format!("{}/content", provider.domain()),"data":item})
        };
        if let Some(data) = part["data"].as_str().filter(|s| s.len() > 65_536) {
            if matches!(part["type"].as_str(), Some("image" | "audio")) {
                if let Ok(decoded) = STANDARD.decode(data) {
                    part["blob"] = bytes(
                        spool,
                        &decoded,
                        part["mimeType"]
                            .as_str()
                            .unwrap_or("application/octet-stream"),
                    )?;
                    part.as_object_mut().map(|o| o.remove("data"));
                }
            }
        }
        parts.push(part);
    }
    record["content"] = json!(parts);
    Ok(())
}
pub(super) fn image(spool: &Spool, url: Option<&str>, mime: Option<&str>) -> Result<Option<Value>> {
    let Some(url) = url else {
        return Ok(None);
    };
    if let Some((kind, data)) = url
        .strip_prefix("data:")
        .and_then(|s| s.split_once(";base64,"))
    {
        let mut part = json!({"type":"image","mimeType":kind,"data":data});
        if let Ok(decoded) = STANDARD.decode(data) {
            if decoded.len() > 65_536 {
                part["blob"] = bytes(spool, &decoded, kind)?;
                part.as_object_mut().map(|o| o.remove("data"));
            }
        }
        return Ok(Some(part));
    }
    let mut p = json!({"type":"resource_link","name":"image","uri":url});
    if let Some(mime) = mime {
        p["mimeType"] = json!(mime);
    }
    Ok(Some(p))
}
pub(super) fn time(value: &Value, millis: bool) -> Option<String> {
    if let Some(raw) = value.as_str() {
        return crate::records::timestamp(raw);
    }
    let n = value.as_i64()?;
    let time = if millis {
        chrono::DateTime::from_timestamp_millis(n)
    } else {
        chrono::DateTime::from_timestamp(n, 0)
    }?;
    Some(time.to_rfc3339_opts(
        if millis {
            chrono::SecondsFormat::Millis
        } else {
            chrono::SecondsFormat::Secs
        },
        true,
    ))
}

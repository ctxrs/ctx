use super::*;
use std::io::Write;
pub(super) fn part(p: &Value, provider: crate::Provider) -> Option<Value> {
    if p["ignored"] == true {
        return None;
    }
    let mut r = match p["type"].as_str().unwrap_or("") {
        "text" => json!({"type":"text","text":p["text"].as_str()?}),
        "reasoning" => json!({"type":"reasoning","visibility":"full","text":p["text"].as_str()?}),
        "file" => {
            let uri = p["url"].as_str().or_else(|| p["uri"].as_str())?;
            if let Some((mime, data)) = uri
                .strip_prefix("data:")
                .and_then(|s| s.split_once(";base64,"))
            {
                if mime.starts_with("image/") || mime.starts_with("audio/") {
                    json!({"type":if mime.starts_with("image/"){"image"}else{"audio"},"mimeType":mime,"data":data})
                } else {
                    json!({"type":"resource","resource":{"uri":uri,"mimeType":mime,"blob":data}})
                }
            } else {
                json!({"type":"resource_link","uri":uri,"name":p["filename"].as_str().or_else(||p["name"].as_str()).unwrap_or("file")})
            }
        }
        _ => return None,
    };
    if p["synthetic"] == true {
        r["author"] = json!("harness");
        r["origin"] = json!("injected");
    }
    // Keep provenance, ignored flags and provider-specific reasoning state.
    let mut extra = p.clone();
    if let Some(o) = extra.as_object_mut() {
        for key in ["type", "text", "url", "uri"] {
            o.remove(key);
        }
    }
    meta(&mut r, provider, "part", &extra);
    Some(r)
}

pub(super) fn fold(s: &mut Spool, w: &Writer, r: &mut Value, id: &str, d: &Value) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(&s.bundle)?;
    file.write_all(b"[")?;
    let mut size = 0;
    let mut count = 0;
    let mut pos = 0;
    let mut small = Vec::new();
    loop {
        let next:Option<u64>=s.db.query_row("SELECT pos FROM rows WHERE response=?1 AND kind='part' AND pos>?2 ORDER BY pos LIMIT 1",rusqlite::params![id,pos],|r|r.get(0)).optional()?;
        let Some(next) = next else {
            break;
        };
        pos = next;
        let p = s.get(pos)?;
        if let Some(part) = part(&p["data"], w.provider) {
            if count > 0 {
                file.write_all(b",")?;
            }
            let bytes = serde_json::to_vec(&part)?;
            size += bytes.len();
            file.write_all(&bytes)?;
            count += 1;
            if size <= s.limit {
                small.push(part);
            } else {
                small.clear();
            }
        }
    }
    if count == 0 {
        let native = d.get("content").or_else(|| d.get("text"));
        if let Some(native) = native {
            r["content"] = json!(crate::content::parts(native, w.provider));
        } else {
            r["capture"]["content"] = json!({"state":"absent"});
        }
    } else if size <= s.limit {
        r["content"] = json!(small);
    } else {
        file.write_all(b"]")?;
        r["capture"]["content"] = json!({"state":"external","blob":s.blob(file)?});
    }
    Ok(())
}

pub(super) fn output(v: &Value, provider: crate::Provider) -> Vec<Value> {
    if v.is_string() {
        return crate::content::parts(v, provider);
    }
    v.as_array()
        .into_iter()
        .flatten()
        .map(|p| part(p, provider).unwrap_or_else(|| json!({"type":"data","data":p})))
        .collect()
}

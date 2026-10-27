//! Secret answers are removed before any native/vendor/blob projection (ASR-283).
use crate::{spool::Spool, Result};
use rusqlite::params;
use serde_json::Value;

pub fn questions(spool: &Spool, v: &Value) -> Result<()> {
    match v {
        Value::Object(o) => {
            if o.get("secret") == Some(&Value::Bool(true))
                || o.get("isSecret") == Some(&Value::Bool(true))
            {
                if let Some(id) = o.get("requestId").and_then(Value::as_str) {
                    spool
                        .db
                        .execute("INSERT OR IGNORE INTO secret_questions VALUES(?1)", [id])?;
                }
            }
            if let Some(qs) = o.get("questions").and_then(Value::as_array) {
                for (i, q) in qs.iter().enumerate() {
                    if q["secret"] == true || q["isSecret"] == true {
                        let qid = q["id"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| i.to_string());
                        spool
                            .db
                            .execute("INSERT OR IGNORE INTO secret_questions VALUES(?1)", [qid])?;
                        if let Some(text) = q["question"].as_str() {
                            spool.db.execute(
                                "INSERT OR IGNORE INTO secret_questions VALUES(?1)",
                                [text],
                            )?;
                        }
                    }
                }
            }
            for (key, x) in o {
                if matches!(key.as_str(), "arguments" | "arguments_json" | "text") {
                    if let Some(s) = x.as_str() {
                        if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                            questions(spool, &parsed)?;
                        }
                    }
                }
                questions(spool, x)?;
            }
        }
        Value::Array(a) => {
            for x in a {
                questions(spool, x)?;
            }
        }
        Value::String(s) if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') => {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                questions(spool, &parsed)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn strings(spool: &Spool, v: &Value) -> Result<()> {
    match v {
        Value::String(s) if !s.is_empty() => {
            spool
                .db
                .execute("INSERT OR IGNORE INTO secrets VALUES(?1)", [s])?;
        }
        Value::Array(a) => {
            for x in a {
                strings(spool, x)?;
            }
        }
        Value::Object(o) => {
            for x in o.values() {
                strings(spool, x)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn answers(spool: &Spool, v: &Value) -> Result<()> {
    match v {
        Value::Object(o) => {
            if let Some(id) = o.get("requestId").and_then(Value::as_str) {
                let secret: bool = spool.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM secret_questions WHERE id=?1)",
                    [id],
                    |r| r.get(0),
                )?;
                if secret {
                    for key in ["answer", "response"] {
                        if let Some(value) = o.get(key) {
                            strings(spool, value)?;
                        }
                    }
                }
            }
            if let Some(a) = o.get("answers").and_then(Value::as_object) {
                for (key, value) in a {
                    let secret: bool = spool.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM secret_questions WHERE id=?1)",
                        [key],
                        |r| r.get(0),
                    )?;
                    if secret {
                        strings(spool, value)?;
                    }
                }
            }
            for x in o.values() {
                answers(spool, x)?;
            }
        }
        Value::Array(a) => {
            for x in a {
                answers(spool, x)?;
            }
        }
        Value::String(s) if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') => {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                answers(spool, &parsed)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn prepare(spool: &mut Spool) -> Result<()> {
    let partial = spool.entity("journal:partial")?;
    if let Some(p) = &partial {
        questions(spool, p)?;
    }
    let has_questions: bool =
        spool
            .db
            .query_row("SELECT EXISTS(SELECT 1 FROM secret_questions)", [], |r| {
                r.get(0)
            })?;
    if !has_questions {
        return Ok(());
    }
    let mut pos = 0;
    while let Some(next) = spool.next(pos)? {
        pos = next;
        let value = spool.get(pos)?;
        answers(spool, &value)?;
    }
    if let Some(p) = &partial {
        answers(spool, p)?;
    }
    spool.has_secrets = spool
        .db
        .query_row("SELECT EXISTS(SELECT 1 FROM secrets)", [], |r| r.get(0))?;
    Ok(())
}
pub fn sanitize(spool: &Spool, v: &mut Value) -> Result<()> {
    if !spool.has_secrets {
        return Ok(());
    }
    match v {
        Value::String(s) => {
            if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') {
                // Redact JSON string tokens without parsing/re-serializing numbers:
                // an unrelated secret answer must not round an unsafe native integer.
                let bytes = s.as_bytes();
                let (mut i, mut copied) = (0, 0);
                let mut output = String::new();
                while i < bytes.len() {
                    if bytes[i] != b'"' {
                        i += 1;
                        continue;
                    }
                    let start = i;
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    if i >= bytes.len() {
                        break;
                    }
                    i += 1;
                    if let Ok(text) = serde_json::from_str::<String>(&s[start..i]) {
                        let mut token = Value::String(text.clone());
                        sanitize(spool, &mut token)?;
                        if token.as_str() != Some(text.as_str()) {
                            output.push_str(&s[copied..start]);
                            output.push_str(&serde_json::to_string(&token)?);
                            copied = i;
                        }
                    }
                }
                if copied > 0 {
                    output.push_str(&s[copied..]);
                    *s = output;
                    return Ok(());
                }
            }
            let mut stmt = spool.db.prepare_cached(
                "SELECT value FROM secrets WHERE instr(?1,value)>0 ORDER BY length(value) DESC",
            )?;
            let values = stmt.query_map(params![&*s], |r| r.get::<_, String>(0))?;
            for value in values {
                *s = s.replace(&value?, "[secret answer redacted]");
            }
        }
        Value::Array(a) => {
            for x in a {
                sanitize(spool, x)?;
            }
        }
        Value::Object(o) => {
            for x in o.values_mut() {
                sanitize(spool, x)?;
            }
        }
        _ => {}
    }
    Ok(())
}

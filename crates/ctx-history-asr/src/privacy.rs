//! Secret answers are removed before any native/vendor/blob projection (ASR-283).
use crate::{spool::Spool, Result};
use rusqlite::params;
use serde_json::Value;

pub(crate) fn questions(spool: &Spool, v: &Value) -> Result<()> {
    match v {
        Value::Object(o) => {
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
pub(crate) fn answers(spool: &Spool, v: &Value) -> Result<()> {
    match v {
        Value::Object(o) => {
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
pub(crate) fn prepare(spool: &mut Spool) -> Result<()> {
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
    spool.has_secrets = spool
        .db
        .query_row("SELECT EXISTS(SELECT 1 FROM secrets)", [], |r| r.get(0))?;
    Ok(())
}
pub(crate) fn sanitize(spool: &Spool, v: &mut Value) -> Result<()> {
    if !spool.has_secrets {
        return Ok(());
    }
    match v {
        Value::String(s) => {
            if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') {
                if let Ok(mut parsed) = serde_json::from_str::<Value>(s) {
                    let original = parsed.clone();
                    sanitize(spool, &mut parsed)?;
                    if parsed != original {
                        *s = serde_json::to_string(&parsed)?;
                        return Ok(());
                    }
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

use crate::{
    genai,
    records::{extras, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn segment(spool: &Spool) -> Result<()> {
    spool.db.execute_batch("CREATE TABLE IF NOT EXISTS gemini_active(native TEXT PRIMARY KEY,pos INTEGER,attachment TEXT); CREATE TABLE IF NOT EXISTS gemini_nodes(native TEXT,record TEXT PRIMARY KEY); DELETE FROM gemini_active;")?;
    spool.save("gemini-segment-start", &json!(true))?;
    Ok(())
}

pub(crate) fn convert(spool: &mut Spool, w: &mut Writer, v: &Value) -> Result<()> {
    if v["type"] == "_gap" {
        w.emit(json!({"type":"gap","reason":v["reason"],"count":1}), v)?;
        return Ok(());
    }
    spool.db.execute_batch("CREATE TABLE IF NOT EXISTS gemini_active(native TEXT PRIMARY KEY,pos INTEGER,attachment TEXT); CREATE TABLE IF NOT EXISTS gemini_nodes(native TEXT,record TEXT PRIMARY KEY);")?;
    if let Some(target) = v["$rewindTo"].as_str() {
        let before: Option<u64> = spool
            .db
            .query_row(
                "SELECT pos FROM gemini_active WHERE native=?1",
                [target],
                |r| r.get(0),
            )
            .optional()?;
        let to: Option<String> = if let Some(before) = before {
            let prev = spool
                .db
                .query_row(
                    "SELECT attachment FROM gemini_active WHERE pos<?1 ORDER BY pos DESC LIMIT 1",
                    [before],
                    |r| r.get(0),
                )
                .optional()?;
            spool
                .db
                .execute("DELETE FROM gemini_active WHERE pos>=?1", [before])?;
            prev
        } else {
            spool.db.execute("DELETE FROM gemini_active", [])?;
            None
        };
        let mut r = json!({"type":"rewind","to":to,"reason":"other"});
        meta(&mut r, Provider::Gemini, "rewindTo", &json!(target));
        w.emit(r, v)?;
        return Ok(());
    }
    if let Some(update) = v.get("$set") {
        if let Some(messages) = update["messages"].as_array() {
            // Masking synchronization is a full membership snapshot. Prune precisely
            // the removed native units; do not call an unmarked new message a summary.
            spool
                .db
                .execute_batch("CREATE TEMP TABLE checkpoint_ids(id TEXT PRIMARY KEY);")?;
            for m in messages {
                if let Some(id) = m["id"].as_str() {
                    spool
                        .db
                        .execute("INSERT OR IGNORE INTO checkpoint_ids VALUES(?1)", [id])?;
                }
            }
            let mut removed = Vec::<String>::new();
            let mut removed_bytes = 0;
            let mut overflow = false;
            let mut stmt = spool.db.prepare("SELECT record FROM gemini_nodes WHERE native IN (SELECT native FROM gemini_active WHERE native NOT IN (SELECT id FROM checkpoint_ids)) ORDER BY rowid")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                removed_bytes += id.len() + 3;
                if removed_bytes > spool.limit / 2 {
                    overflow = true;
                    break;
                }
                removed.push(id);
            }
            drop(rows);
            drop(stmt);
            if !removed.is_empty() {
                let mut r = json!({"type":"compaction","compactionId":format!("checkpoint:{}",w.pos),"idSource":"derived","kind":"prune","status":"completed","replaces":{"ids":removed}});
                if overflow {
                    r["replaces"] = json!({"state":"unknown"});
                }
                meta(&mut r, Provider::Gemini, "historyCheckpoint", &json!(true));
                w.emit(r, v)?;
            }
            spool.db.execute(
                "DELETE FROM gemini_active WHERE native NOT IN (SELECT id FROM checkpoint_ids)",
                [],
            )?;
            spool.db.execute_batch("DROP TABLE checkpoint_ids;")?;
            for m in messages {
                message(spool, w, m)?;
            }
            let mut native =
                json!({"type":format!("{}/history-snapshot",Provider::Gemini.domain())});
            native["native"] = json!({"record":v});
            w.emit(native, &json!({}))?;
        }
        metadata(spool, w, update)?;
    } else if v["sessionId"].is_string() {
        metadata(spool, w, v)?;
    } else if v["id"].is_string() {
        message(spool, w, v)?;
    } else {
        w.vendor(v, "unknown")?;
    }
    Ok(())
}
fn metadata(spool: &Spool, w: &mut Writer, v: &Value) -> Result<()> {
    let mut r = spool
        .entity("metadata")?
        .unwrap_or_else(|| json!({"type":"session_info"}));
    r.as_object_mut().unwrap().remove("_record");
    r.as_object_mut().unwrap().remove("supersedes");
    if let Some(title) = v.get("summary") {
        r["title"] = title.clone();
    }
    if v["projectHash"].is_string() {
        r["project"] = json!({"id":v["projectHash"]});
    }
    extras(
        &mut r,
        Provider::Gemini,
        v,
        &["messages", "summary", "projectHash", "_snapshot"],
    );
    genai::revision(spool, w, "metadata", r, &json!({}))?;
    Ok(())
}
fn message(spool: &Spool, w: &mut Writer, v: &Value) -> Result<()> {
    let Some(id) = v["id"].as_str() else {
        w.vendor(v, "unknown-message")?;
        return Ok(());
    };
    spool.save("gemini-current", &json!(id))?;
    let mut r = json!({"type":"message","messageId":id,"content":genai::parts(&v["content"],Provider::Gemini)});
    match v["type"].as_str() {
        Some("gemini") => {
            r["role"] = json!("assistant");
            r["author"] = json!("model");
        }
        Some("user") => r["role"] = json!("user"),
        Some("info" | "error" | "warning") => {
            r["author"] = json!("harness");
            r["origin"] = json!("notice");
            r["audience"] = json!("user");
            r["capture"] = json!({"role":{"state":"absent"}});
            meta(&mut r, Provider::Gemini, "messageType", &v["type"]);
        }
        _ => {
            w.vendor(v, "unknown-message")?;
            return Ok(());
        }
    }
    if v.get("content").is_none() {
        r["capture"]["content"] = json!({"state":"absent"});
    }
    for thought in v["thoughts"].as_array().into_iter().flatten() {
        let mut p = json!({"type":"reasoning","visibility":"summary"});
        if let Some(text) = thought["description"].as_str() {
            p["summary"] = json!([text]);
        }
        extras(&mut p, Provider::Gemini, thought, &["description"]);
        if let Some(ts) = thought.get("timestamp") {
            meta(&mut p, Provider::Gemini, "timestamp", ts);
        }
        r["content"].as_array_mut().unwrap().push(p);
    }
    if let Some(display) = v.get("displayContent") {
        r["displayContent"] = json!(genai::parts(display, Provider::Gemini));
        r["displayCorrespondence"] = json!("native");
    }
    extras(
        &mut r,
        Provider::Gemini,
        v,
        &[
            "id",
            "content",
            "displayContent",
            "thoughts",
            "toolCalls",
            "tokens",
            "_snapshot",
        ],
    );
    if v["_snapshot"] == true {
        r["idScope"] = json!("capture");
    }
    if let Some(parts) = v["content"].as_array() {
        let functions: Vec<_> = parts
            .iter()
            .filter(|p| p.get("functionCall").is_some() || p.get("functionResponse").is_some())
            .collect();
        if !functions.is_empty() {
            meta(&mut r, Provider::Gemini, "functionParts", &json!(functions));
        }
    }
    if spool.entity(&format!("message:{id}"))?.is_none()
        && spool.entity("gemini-segment-start")? == Some(json!(true))
    {
        let predecessor: Option<String> = spool
            .db
            .query_row(
                "SELECT attachment FROM gemini_active ORDER BY pos DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if predecessor != w.tip {
            r["branchesFrom"] = json!({"record":predecessor});
        }
        spool.save("gemini-segment-start", &json!(false))?;
    }
    let record = genai::revision(spool, w, &format!("message:{id}"), r, v)?;
    let mut attachment = record;
    for (i, tc) in v["toolCalls"].as_array().into_iter().flatten().enumerate() {
        let derived = format!("call:{id}:{i}");
        let call = genai::call(tc, &derived, Provider::Gemini);
        let cid = call["callId"].as_str().unwrap().to_owned();
        attachment = genai::revision(spool, w, &format!("call:{id}:{cid}"), call, tc)?;
        if tc.get("result").is_some_and(|r| !r.is_null())
            || matches!(
                tc["status"].as_str(),
                Some("success" | "error" | "cancelled")
            )
        {
            let result = genai::result(spool, Some(&cid), tc.get("result"), tc, Provider::Gemini)?;
            attachment =
                genai::revision(spool, w, &format!("result:{id}:{cid}"), result, &json!({}))?;
            spool.save(&format!("gemini-result:{cid}"), &json!(true))?;
        }
    }
    for (i, p) in v["content"].as_array().into_iter().flatten().enumerate() {
        if let Some(fc) = p.get("functionCall") {
            let native_id = fc["id"].as_str();
            let embedded = native_id.is_some_and(|id| {
                v["toolCalls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|tc| tc["id"] == id)
            });
            if !embedded {
                let call = genai::call(fc, &format!("call:{id}:{i}"), Provider::Gemini);
                let cid = call["callId"].as_str().unwrap().to_owned();
                attachment = genai::revision(spool, w, &format!("call:{id}:{cid}"), call, v)?;
            }
        }
    }
    // History snapshots may include functionResponse user turns as well as the
    // same result nested in toolCalls. Keep their native representation in meta,
    // but do not count that second storage representation as another tool result.
    if let Some(a) = v["content"].as_array() {
        for p in a {
            if let Some(fr) = p.get("functionResponse") {
                let cid = fr["id"].as_str();
                let known = if let Some(cid) = cid {
                    !spool.ambiguous_call(cid)?
                        && spool.entity(&format!("gemini-result:{cid}"))?.is_some()
                } else {
                    false
                };
                if !known {
                    let result =
                        genai::result(spool, cid, Some(&json!([p])), &json!({}), Provider::Gemini)?;
                    attachment = genai::revision(
                        spool,
                        w,
                        &format!("orphan:{id}:{}", cid.unwrap_or("none")),
                        result,
                        v,
                    )?;
                }
            }
        }
    }
    spool.db.execute("INSERT INTO gemini_active VALUES(?1,?2,?3) ON CONFLICT(native) DO UPDATE SET attachment=excluded.attachment",params![id,w.count,attachment])?;
    genai::usage(spool, w, v, id)?;
    Ok(())
}

//! Pi v4 has one ordered mutation journal and independently movable lane heads.
use super::*;
use rusqlite::OptionalExtension;
pub(crate) fn index(spool: &Spool, v: &Value) -> Result<()> {
    if v["kind"] == "record"
        && v["type"] == "usage"
        && matches!(
            v["cause"].as_str(),
            Some("assistant" | "compaction" | "branch_summary" | "deferred_fetch")
        )
    {
        if let Some(entry) = v["entryId"].as_str() {
            spool.save(&format!("pi:usage:{entry}"), v)?;
        }
    }
    Ok(())
}
pub(super) fn before(spool: &mut Spool, w: &mut Writer, v: &Value) -> Result<bool> {
    let kind = v["kind"].as_str().unwrap_or("");
    let lane = v["lane"].as_str().unwrap_or("main");
    w.turn = spool
        .entity(&format!("pi:run:{lane}"))?
        .and_then(|v| v.as_str().map(str::to_owned));
    match kind {
        "header" => return Ok(true),
        "entry" => {
            if let Some(id) = v["id"].as_str() {
                spool.save(&format!("pi:lane:{lane}"), &json!(id))?;
            }
            return Ok(false);
        }
        "lane" => spool.save(&format!("pi:lane:{lane}"), &v["leafId"])?,
        "fact" if v["fact"] == "name" && v["name"].is_string() => {
            let mut r = json!({"type":"session_info","title":v["name"],"native":{"record":v}});
            meta(&mut r, P, "lane", &json!(lane));
            w.emit(r, v)?;
            return Ok(true);
        }
        "record" => {
            match v["type"].as_str() {
                Some("operation_started") => {
                    w.turn = v["id"].as_str().map(str::to_owned);
                    spool.save(&format!("pi:run:{lane}"), &v["id"])?;
                    w.emit(
                        json!({"type":"state","state":"running","native":{"record":v}}),
                        v,
                    )?;
                    return Ok(true);
                }
                Some("operation_finished") => {
                    w.turn = v["runId"].as_str().map(str::to_owned);
                    let mut r = json!({"type":"state","state":"idle","native":{"record":v}});
                    if let Some(reason) = match v["outcome"].as_str() {
                        Some("completed") => Some("end_turn"),
                        Some("aborted") => Some("cancelled"),
                        Some("failed") => Some("error"),
                        _ => None,
                    } {
                        r["stopReason"] = json!(reason);
                    }
                    copy(&mut r, "error", v, "error");
                    w.emit(r, v)?;
                    spool.save(&format!("pi:run:{lane}"), &Value::Null)?;
                    w.turn = None;
                    return Ok(true);
                }
                Some("usage") => {
                    let model_usage = matches!(
                        v["cause"].as_str(),
                        Some("assistant" | "compaction" | "branch_summary" | "deferred_fetch")
                    );
                    let mut r = if model_usage {
                        json!({"type":"inference","inferenceId":v["id"],"purpose":match v["cause"].as_str(){Some("compaction")=>"compaction",Some("branch_summary")=>"summary",_=>"turn"}})
                    } else {
                        json!({"type":"usage_summary","scope":"session","cumulative":false,"includesChildren":false})
                    };
                    r["usage"] = jsonl::usage(&v["usage"], true);
                    r["native"] = json!({"record":v});
                    if model_usage {
                        copy(&mut r, "stopReason", v, "stopReason");
                        let pos:Option<u64>=spool.db.query_row("SELECT pos FROM rows WHERE native_id=?1 AND kind='message' LIMIT 1",[v["entryId"].as_str().unwrap_or("")],|r|r.get(0)).optional()?;
                        if let Some(pos) = pos {
                            let entry = spool.get(pos)?;
                            let m = &entry["message"];
                            if m["model"].is_string() {
                                r["model"] = json!({"id":m["model"]});
                                copy(&mut r["model"], "provider", m, "provider");
                            }
                        }
                    }
                    if let Some(cost) = v.pointer("/usage/cost/total").and_then(Value::as_f64) {
                        r["cost"] =
                            json!({"amount":cost,"currency":"USD","source":"harness_estimated"});
                    }
                    w.emit(r, v)?;
                    return Ok(true);
                }
                _ => {}
            }
        }
        _ => return Ok(false),
    }
    let mut r = json!({"type":format!("{}/{}",P.domain(),if kind=="record"{v["type"].as_str().unwrap_or("record")}else{kind}),"native":{"record":v}});
    meta(&mut r, P, "lane", &json!(lane));
    w.emit(r, v)?;
    Ok(true)
}
pub(super) fn finish(spool: &Spool, w: &mut Writer) -> Result<()> {
    let mut heads = Vec::new();
    let mut native = json!({});
    let mut stmt = spool
        .db
        .prepare("SELECT key,value FROM entities WHERE key LIKE 'pi:lane:%' ORDER BY key")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let value: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
        native[&key[8..]] = value.clone();
        if let Some(id) = value.as_str() {
            if let Some(target) =
                crate::tree::native_parent(spool, &json!({"parentId":id}), "parentId")?
            {
                heads.push(json!(target));
            }
        }
    }
    if !native.as_object().unwrap().is_empty() {
        w.position(w.pos + 1);
        w.turn = None;
        let mut r = json!({"type":"session_info","heads":heads});
        meta(&mut r, P, "lanes", &native);
        w.emit(r, &Value::Null)?;
    }
    Ok(())
}

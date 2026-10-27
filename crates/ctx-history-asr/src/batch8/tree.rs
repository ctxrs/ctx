//! ASR-361 over native units, with attachment points kept in the disk index.
use crate::{records::Writer, spool::Spool, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(super) fn resolve(spool: &Spool, native: Option<&str>) -> Result<(Option<String>, bool)> {
    let mut parent = native.map(str::to_owned);
    let mut steps = 0;
    while let Some(id) = parent {
        if let Some(output) = spool.output(&id)? {
            return Ok((Some(output), true));
        }
        let row: Option<(Option<String>, String)> = spool
            .db
            .query_row(
                "SELECT parent,kind FROM rows WHERE native_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        steps += 1;
        let Some((next, kind)) = row else {
            return Ok((None, false));
        };
        // Unemitted context units are not transparent audit ancestors.
        if kind == "devin" || steps > 100_000 {
            return Ok((None, false));
        }
        parent = next;
    }
    Ok((None, true))
}

pub(super) fn emit(
    spool: &Spool,
    writer: &mut Writer,
    mut record: Value,
    unit: &Value,
    source: &Value,
    first: bool,
) -> Result<()> {
    let injected = matches!(record["author"].as_str(), Some("harness" | "hook"))
        || matches!(
            record["origin"].as_str(),
            Some("injected" | "memory" | "retrieval" | "notice")
        );
    let failed = record["type"] == "message"
        && record["role"] == "assistant"
        && matches!(record["status"].as_str(), Some("error" | "interrupted"));
    let mut response = unit["response"].as_str().map(str::to_owned);
    if matches!(record["type"].as_str(), Some("tool_result" | "tool_call")) {
        if let Some(call) = record["callId"].as_str() {
            response = spool
                .entity(&format!("batch8:response:{call}"))?
                .and_then(|v| v.as_str().map(str::to_owned));
        }
    }
    let mut predecessor = writer.tip.clone();
    if first {
        let (parent, known) = resolve(spool, unit["parent"].as_str())?;
        if !known {
            record["branchesFrom"] = json!({"state":"unknown"});
            predecessor = None;
        } else if parent != writer.tip {
            // Membership is tested before M2: injection cannot turn an off-path
            // parent into a linear edge.
            let parent_response = if let Some(id) = &parent {
                spool
                    .db
                    .query_row("SELECT response FROM structure WHERE id=?1", [id], |r| {
                        r.get::<_, Option<String>>(0)
                    })?
            } else {
                None
            };
            let mut cursor = writer.tip.clone();
            let mut mechanical = true;
            while cursor != parent {
                let Some(ref id) = cursor else {
                    mechanical = false;
                    break;
                };
                let node: Option<(Option<String>, Option<String>, bool)> = spool
                    .db
                    .query_row(
                        "SELECT parent,response,mechanical FROM structure WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                let Some((prev, rk, special)) = node else {
                    mechanical = false;
                    break;
                };
                if !injected
                    && !special
                    && !(rk.is_some() && (rk == response || rk == parent_response))
                {
                    mechanical = false;
                }
                cursor = prev;
            }
            if parent.is_none() || cursor != parent || !mechanical {
                record["branchesFrom"] = json!({"record":parent});
                predecessor = parent;
            }
        }
    }
    if matches!(record["type"].as_str(), Some("tool_result" | "tool_call")) {
        if let Some(call) = record["callId"].as_str() {
            let mut retained = record.clone();
            retained["_record"] = json!(writer.id());
            let kind = if record["type"] == "tool_call" {
                "call"
            } else {
                "result"
            };
            spool.save(&format!("batch8:{kind}:{call}"), &retained)?;
        }
    }
    let id = writer.emit(record, source)?;
    spool.db.execute(
        "INSERT INTO structure VALUES(?1,?2,?3,?4)",
        params![id, predecessor, response, injected || failed],
    )?;
    if let Some(node) = unit["node"].as_str() {
        spool.set_output(node, &id)?;
    }
    Ok(())
}

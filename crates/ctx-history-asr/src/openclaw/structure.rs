use crate::{native as n, records::Writer, spool::Spool, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

/// None is unresolved; JSON null is a positively recorded root.
pub(super) fn attachment(s: &Spool, id: &Value) -> Result<Option<Value>> {
    if id.is_null() {
        return Ok(Some(Value::Null));
    }
    let mut cursor = id.as_str().map(str::to_owned);
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            break;
        }
        if let Some(out) = s.output(&id)? {
            return Ok(Some(json!(out)));
        }
        let parent: Option<Option<String>> =
            s.db.query_row(
                "SELECT parent FROM rows WHERE native_id=?1 AND kind='transcript_events' LIMIT 1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        match parent {
            Some(None) => return Ok(Some(Value::Null)),
            Some(Some(p)) => cursor = Some(p),
            None => break,
        }
    }
    Ok(None)
}
pub(super) fn rewrite(s: &Spool, e: &Value, r: &mut Value) -> Result<()> {
    let Some(old) = e["supersedesEntryId"].as_str() else {
        return Ok(());
    };
    let Some(unit) = s.entity(&format!("unit:{old}"))? else {
        n::loss(s, "revision_history")?;
        return Ok(());
    };
    let key = match r["type"].as_str() {
        Some("message") => "messageId",
        Some("tool_call" | "tool_result") => "callId",
        Some("inference") => "inferenceId",
        _ => return Ok(()),
    };
    if let Some(prev) = unit
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["type"] == r["type"] && v[key] == r[key])
    {
        r["supersedes"] = prev["_record"].clone();
        r["assembly"] = json!({"mode":"rewrite","sourceIds":[e["id"]]});
    }
    Ok(())
}
pub(super) fn place(
    s: &Spool,
    w: &Writer,
    e: &Value,
    r: &mut Value,
    response: Option<&str>,
    first: bool,
) -> Result<()> {
    let parent = if first {
        attachment(s, &e["parentId"])?
    } else {
        Some(json!(w.tip))
    };
    let injected = matches!(r["author"].as_str(), Some("harness" | "hook"))
        || matches!(
            r["origin"].as_str(),
            Some("injected" | "memory" | "retrieval" | "notice")
        );
    let mut predecessor = w.tip.clone();
    if first && parent != Some(json!(w.tip)) {
        let resolved = parent.as_ref().and_then(Value::as_str).map(str::to_owned);
        let mut mechanical = false;
        if resolved.is_some() {
            let pr: Option<String> =
                s.db.query_row(
                    "SELECT response FROM structure WHERE id=?1",
                    [resolved.as_deref()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let mut node = w.tip.clone();
            let mut seen = std::collections::HashSet::new();
            mechanical = true;
            while node != resolved {
                let Some(id) = node.filter(|id| seen.insert(id.clone())) else {
                    mechanical = false;
                    break;
                };
                let row: Option<(Option<String>, Option<String>, bool)> =
                    s.db.query_row(
                        "SELECT parent,response,mechanical FROM structure WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                let Some((p, rk, special)) = row else {
                    mechanical = false;
                    break;
                };
                if !injected
                    && !special
                    && !(rk.is_some() && (rk.as_deref() == response || rk == pr))
                {
                    mechanical = false;
                    break;
                }
                node = p;
            }
        }
        if !mechanical || r.get("supersedes").is_some() {
            predecessor = resolved;
        }
    }
    if let Some(old) = r["supersedes"].as_str() {
        let node = s.entity(&format!("node:{old}"))?.unwrap_or(json!(old));
        let implicit: Option<Option<String>> =
            s.db.query_row(
                "SELECT parent FROM structure WHERE id=?1",
                [node.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        // Revisions keep their node position. Mark the native parent when it differs.
        if implicit.flatten() != predecessor {
            r["branchesFrom"] = json!({"record":predecessor});
        }
    } else if predecessor != w.tip {
        r["branchesFrom"] = json!({"record":predecessor});
    }
    if first && parent.is_none() {
        r["branchesFrom"] = json!({"state":"unknown"});
        n::loss(s, "branches")?;
    }
    s.save("predecessor", &json!(predecessor))
}
pub(super) fn remember(
    s: &Spool,
    w: &mut Writer,
    e: &Value,
    r: &Value,
    out: &str,
    response: Option<&str>,
) -> Result<()> {
    let node = if let Some(old) = r["supersedes"].as_str() {
        s.entity(&format!("node:{old}"))?.unwrap_or(json!(old))
    } else {
        json!(out)
    };
    let predecessor = s.entity("predecessor")?.unwrap_or(Value::Null);
    let special = matches!(r["author"].as_str(), Some("harness" | "hook"))
        || matches!(r["status"].as_str(), Some("error" | "interrupted"));
    s.db.execute(
        "INSERT OR REPLACE INTO structure VALUES(?1,?2,?3,?4)",
        params![node.as_str(), predecessor.as_str(), response, special],
    )?;
    s.save(&format!("node:{out}"), &node)?;
    w.tip = node.as_str().map(str::to_owned);
    if let (Some(native), Some(node)) = (e["id"].as_str(), node.as_str()) {
        s.set_output(native, node)?;
    }
    Ok(())
}
pub(super) fn compaction(s: &Spool, e: &Value) -> Result<Value> {
    let parent = attachment(s, &e["parentId"])?;
    let kept = attachment(s, &e["firstKeptEntryId"])?;
    let mut r = json!({"type":"compaction","compactionId":e["id"],"kind":if e["type"]=="compaction"{"summary"}else{e["type"].as_str().unwrap_or("summary")},"status":"completed","replaces":{"state":"unknown"},"historyRetained":true});
    if e["type"] == "branch_summary" {
        if let (Some(end), Some(branch)) = (parent.clone(), attachment(s, &e["fromId"])?) {
            let mut ancestors = std::collections::HashSet::new();
            let mut cursor = end.as_str().map(str::to_owned);
            while let Some(id) = cursor {
                if !ancestors.insert(id.clone()) {
                    break;
                }
                cursor =
                    s.db.query_row("SELECT parent FROM structure WHERE id=?1", [id], |r| {
                        r.get(0)
                    })
                    .optional()?
                    .flatten();
            }
            let mut abandoned = Vec::new();
            cursor = branch.as_str().map(str::to_owned);
            while let Some(id) = cursor {
                if ancestors.contains(&id) || abandoned.contains(&id) {
                    break;
                }
                abandoned.push(id.clone());
                cursor =
                    s.db.query_row("SELECT parent FROM structure WHERE id=?1", [id], |r| {
                        r.get(0)
                    })
                    .optional()?
                    .flatten();
            }
            abandoned.reverse();
            r["replaces"] = json!({"ids":abandoned});
        }
    } else if let Some(parent) = parent {
        r["replaces"] = if parent.is_null() {
            json!({"ids":[]})
        } else {
            json!({"throughId":parent})
        };
        if let Some(kept) = kept.filter(Value::is_string) {
            r["keptFromId"] = kept.clone();
            r["insertAt"] = json!({"beforeId":kept});
        }
    }
    crate::records::copy(&mut r, "reason", e, "reason");
    if let Some(summary) = e.get("summary") {
        r["summary"] = json!(super::messages::parts(summary));
    } else if s
        .entity("window")?
        .is_some_and(|w| w["agent_harness_id"] == "codex")
        && e["type"] == "compaction"
    {
        r["kind"] = json!("provider_native");
        r["capture"]["summary"] = json!({"state":"absent"});
    }
    for (a, b) in [("before", "tokensBefore"), ("after", "tokensAfter")] {
        if let Some(v) = e.get(b) {
            r["tokens"][a] = v.clone();
        }
    }
    n::retain(
        &mut r,
        super::P,
        "compaction",
        &json!({"fromHook":e["fromHook"],"details":e["details"]}),
    );
    Ok(r)
}

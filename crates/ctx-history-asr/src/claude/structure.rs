use crate::{records::Writer, spool::Spool, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(super) fn emit(
    spool: &Spool,
    writer: &mut Writer,
    mut record: Value,
    source: &Value,
    response: Option<&str>,
    first: bool,
) -> Result<()> {
    let injected = matches!(record["author"].as_str(), Some("harness" | "hook"))
        || matches!(
            record["origin"].as_str(),
            Some("injected" | "memory" | "retrieval" | "notice")
        );
    let failed = matches!(record["status"].as_str(), Some("error" | "interrupted"));
    let mut predecessor = writer.tip.clone();
    if first && source.get("parentUuid").is_some() {
        let mut parent = source["parentUuid"]
            .as_str()
            .or_else(|| source["logicalParentUuid"].as_str())
            .map(str::to_owned);
        let mut resolved = None;
        let mut steps = 0;
        while let Some(id) = parent.clone() {
            if let Some(output) = spool.output(&id)? {
                resolved = Some(output);
                break;
            }
            let next: Option<Option<String>> = spool
                .db
                .query_row(
                    "SELECT parent FROM rows WHERE native_id=?1 LIMIT 1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            steps += 1;
            if steps > 100_000 || next.is_none() {
                break;
            }
            parent = next.flatten();
        }
        if resolved != writer.tip {
            let mut mechanical = injected;
            let mut node = writer.tip.clone();
            let parent_response: Option<String> = resolved
                .as_ref()
                .map(|id| {
                    spool
                        .db
                        .query_row("SELECT response FROM structure WHERE id=?1", [id], |r| {
                            r.get(0)
                        })
                })
                .transpose()?
                .flatten();
            if !mechanical && resolved.is_some() {
                mechanical = true;
                while node != resolved {
                    let Some(id) = node else {
                        mechanical = false;
                        break;
                    };
                    let row: Option<(Option<String>, Option<String>, bool)> = spool
                        .db
                        .query_row(
                            "SELECT parent,response,mechanical FROM structure WHERE id=?1",
                            [id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .optional()?;
                    let Some((prev, rk, special)) = row else {
                        mechanical = false;
                        break;
                    };
                    if !special
                        && !(rk.is_some() && (rk.as_deref() == response || rk == parent_response))
                    {
                        mechanical = false;
                        break;
                    }
                    node = prev;
                }
            }
            if !mechanical || resolved.is_none() {
                if parent.is_some() && resolved.is_none() {
                    record["branchesFrom"] = json!({"state":"unknown"});
                } else {
                    record["branchesFrom"] = json!({"record":resolved});
                }
                predecessor = resolved;
            }
        }
    }
    let id = writer.emit(record, source)?;
    spool.db.execute(
        "INSERT INTO structure VALUES(?1,?2,?3,?4)",
        params![id, predecessor, response, injected || failed],
    )?;
    if let Some(native) = source["uuid"].as_str() {
        spool.set_output(native, &id)?;
    }
    Ok(())
}

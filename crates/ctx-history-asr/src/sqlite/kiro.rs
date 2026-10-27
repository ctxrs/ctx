use super::*;

pub(super) fn visit(
    db: &Connection,
    provider: Provider,
    limit: usize,
    staging: &Path,
    namespace: Option<&str>,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    if !table(db, "conversations_v2")? && !table(db, "conversations")? {
        return Err(Error::Invalid(
            "Kiro database has no supported conversation table; ACP/v3 needs its own converter"
                .into(),
        ));
    }
    for t in ["conversations_v2", "conversations"] {
        if !table(db, t)? {
            continue;
        }
        rows(db, t, "ORDER BY key,rowid", &[], limit, &mut |mut row| {
            let legacy = t == "conversations";
            if legacy && namespace.is_none() {
                return Err(Error::Invalid("directory-keyed Kiro conversations require --namespace with a stable installation identity (ASR-054/055); a file path or export timestamp is not a namespace".into()));
            }
            let Some(id) = (if legacy {
                row["key"].as_str().or_else(|| row["_key"].as_str())
            } else {
                row["conversation_id"].as_str()
            })
            .filter(|s| !s.is_empty())
            .map(str::to_owned) else {
                *unidentified += 1;
                return Ok(());
            };
            let mut body = data(&mut row, "value");
            if body.is_none() && row.get("_gap").is_none() {
                row["_gap"] = json!("unparseable");
            }
            let mut h = row.clone();
            h["cwd"] = row["key"].clone();
            h["table"] = json!(t);
            if !legacy {
                h["time_created"] = row["created_at"].clone();
            }
            let mut stage = Stage::new(staging, provider, &id, h)?;
            if let Some(mut body) = body.take() {
                let history = body.as_object_mut().and_then(|o| o.remove("history"));
                stage.push("session", "kiro_state", row, body, None)?;
                match history {
                    Some(Value::Array(entries)) => {
                        for (i, entry) in entries.into_iter().enumerate() {
                            stage.push(
                                &format!("history:{i}"),
                                "kiro_entry",
                                json!({"history_index":i}),
                                entry,
                                None,
                            )?;
                        }
                    }
                    Some(_) => stage.push(
                        "history",
                        "_gap",
                        json!({"_gap":"unparseable"}),
                        Value::Null,
                        None,
                    )?,
                    None => {}
                }
            } else {
                stage.push("session", "_gap", row, Value::Null, None)?;
            }
            stage.finish(visitor)
        })?;
    }
    Ok(())
}

use super::*;

pub(super) fn visit(
    db: &Connection,
    provider: Provider,
    limit: usize,
    staging: &Path,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    let v2 = table(db, "session_v2")?;
    if !v2 && !table(db, "session")? {
        return Err(Error::Invalid(
            "OpenCode-family database has no session table".into(),
        ));
    }
    for name in ["session_v2", "session"] {
        if !table(db, name)? {
            continue;
        }
        let suffix = if name == "session" && v2 {
            "WHERE NOT EXISTS(SELECT 1 FROM session_v2 v WHERE v.id=session.id) ORDER BY time_created,id"
        } else {
            "ORDER BY time_created,id"
        };
        rows(db, name, suffix, &[], limit, &mut |mut h| {
            let Some(id) = h["id"]
                .as_str()
                .or_else(|| h["_key"].as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
            else {
                *unidentified += 1;
                return Ok(());
            };
            h["cwd"] = h["directory"].clone();
            let mut stage = Stage::new(staging, provider, &id, h.clone())?;
            stage.push("session", "session_state", h, Value::Null, None)?;
            if name == "session_v2" && table(db, "session")? {
                rows(db, "session", "WHERE id=?1", &[&id], limit, &mut |row| {
                    stage.push(
                        "legacy_session",
                        "legacy_session_state",
                        row,
                        Value::Null,
                        None,
                    )
                })?;
            }
            let current = if has_rows(db, "session_message", &id)? {
                Some("session_message")
            } else if has_rows(db, "session_entry", &id)? {
                Some("session_entry")
            } else {
                None
            };
            if let Some(current) = current {
                let cols = columns(db, current)?;
                let order = if cols.iter().any(|c| c == "seq") {
                    "seq,id"
                } else {
                    "time_created,id"
                };
                rows(
                    db,
                    current,
                    &format!("WHERE session_id=?1 ORDER BY {order}"),
                    &[&id],
                    limit,
                    &mut |mut row| {
                        let native = key(&row)?;
                        let mut body = data(&mut row, "data")
                            .filter(Value::is_object)
                            .unwrap_or_else(|| json!({"_gap":"unparseable"}));
                        if let Some(t) = row.get("type") {
                            body["type"] = t.clone();
                        }
                        if body.get("_gap").is_some() && row.get("_gap").is_none() {
                            row["_gap"] = json!("unparseable");
                        }
                        stage.push(&format!("message:{native}"), "event", row, body, None)
                    },
                )?;
            }
            if table(db, "message")? {
                rows(
                    db,
                    "message",
                    "WHERE session_id=?1 ORDER BY time_created,id",
                    &[&id],
                    limit,
                    &mut |mut row| {
                        let native = key(&row)?;
                        // v2 migrations retain v1 copies. Preserve their payloads as
                        // alternate representations, without counting messages twice.
                        let migrated = if let Some(t) = current {
                            db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE id=?1 AND session_id=?2)"),params![native,id],|r|r.get::<_,bool>(0))?
                        } else {
                            false
                        };
                        let body = data(&mut row, "data");
                        if body.is_none() && row.get("_gap").is_none() {
                            row["_gap"] = json!("unparseable");
                        }
                        stage.push(
                            &format!("{}:{native}", if migrated { "legacy" } else { "message" }),
                            if migrated {
                                "legacy_representation"
                            } else {
                                "message"
                            },
                            row,
                            body.unwrap_or(Value::Null),
                            None,
                        )?;
                        if table(db, "part")? {
                            rows(
                                db,
                                "part",
                                "WHERE message_id=?1 AND session_id=?2 ORDER BY id",
                                &[&native, &id],
                                limit,
                                &mut |mut row| {
                                    let part = key(&row)?;
                                    let body = data(&mut row, "data");
                                    if body.is_none() && row.get("_gap").is_none() {
                                        row["_gap"] = json!("unparseable");
                                    }
                                    stage.push(
                                        &format!("part:{part}"),
                                        if migrated {
                                            "legacy_representation"
                                        } else {
                                            "part"
                                        },
                                        row,
                                        body.unwrap_or(Value::Null),
                                        Some(&native),
                                    )
                                },
                            )?;
                        }
                        Ok(())
                    },
                )?;
            }
            // Session-owned supporting state stays scoped to its native owner.
            if table(db, "part")? && table(db, "message")? {
                rows(db,"part","WHERE session_id=?1 AND NOT EXISTS(SELECT 1 FROM message m WHERE m.id=part.message_id AND m.session_id=part.session_id) ORDER BY id",&[&id],limit,&mut |mut row|{
                    let key=key(&row)?;
                    let body=data(&mut row,"data").unwrap_or(Value::Null);
                    stage.push(&format!("orphan:{key}"),"orphan_part",row,body,None)
                })?;
            }
            for t in [
                "todo",
                "session_input",
                "session_context_epoch",
                "session_context_snapshot",
            ] {
                if !table(db, t)? || !columns(db, t)?.iter().any(|c| c == "session_id") {
                    continue;
                }
                let mut ordinal = 0;
                rows(
                    db,
                    t,
                    if t == "todo" {
                        "WHERE session_id=?1 ORDER BY position"
                    } else {
                        "WHERE session_id=?1 ORDER BY rowid"
                    },
                    &[&id],
                    limit,
                    &mut |row| {
                        let local = row["id"]
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| row["position"].as_i64().map(|p| p.to_string()))
                            .unwrap_or_else(|| ordinal.to_string());
                        ordinal += 1;
                        stage.push(&format!("{t}:{local}"), t, row, Value::Null, None)
                    },
                )?;
            }
            stage.finish(visitor)
        })?;
    }
    Ok(())
}
fn key(row: &Value) -> Result<String> {
    row["id"]
        .as_str()
        .or_else(|| row["_key"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid("native SQLite row has no bounded identity".into()))
}
fn has_rows(db: &Connection, t: &str, id: &str) -> Result<bool> {
    if !table(db, t)? {
        return Ok(false);
    }
    Ok(db.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE session_id=?1)"),
        [id],
        |r| r.get(0),
    )?)
}

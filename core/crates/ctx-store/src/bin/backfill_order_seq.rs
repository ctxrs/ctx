use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ctx_store::StoreManager;
use serde_json::Value;
use sqlx::Row;

#[derive(Debug, Clone)]
struct EventRow {
    id: String,
    seq: i64,
    turn_id: Option<String>,
    event_type: String,
    payload: Value,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct MessageRow {
    id: String,
    created_at: DateTime<Utc>,
    turn_sequence: Option<i64>,
    order_seq: Option<i64>,
}

#[derive(Debug, Clone)]
struct Appearance {
    key: String,
    created_at: DateTime<Utc>,
    seq: Option<i64>,
    turn_sequence: Option<i64>,
}

fn read_payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = payload.get(*key) {
            if let Some(text) = value.as_str() {
                if !text.trim().is_empty() {
                    return Some(text.trim().to_string());
                }
            }
        }
    }
    None
}

fn read_payload_i64(payload: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(value) = payload.get(*key) {
            if let Some(num) = value.as_i64() {
                return Some(num);
            }
            if let Some(text) = value.as_str() {
                if let Ok(parsed) = text.trim().parse::<i64>() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

fn read_order_seq(payload: &Value) -> Option<i64> {
    read_payload_i64(payload, &["order_seq", "orderSeq"])
}

fn insert_order_seq(payload: &mut Value, order_seq: i64) {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("order_seq".to_string(), Value::Number(order_seq.into()));
    }
}

fn build_item_key(
    event_type: &str,
    payload: &Value,
    turn_id: Option<&str>,
    seq: i64,
) -> Option<String> {
    match event_type {
        "user_message" => {
            let message_id = read_payload_string(payload, &["message_id", "messageId"]);
            message_id.map(|id| format!("message:{id}"))
        }
        "assistant_chunk" | "assistant_complete" | "assistant_message_inserted" => {
            let message_id = read_payload_string(
                payload,
                &[
                    "message_id",
                    "messageId",
                    "provider_message_id",
                    "providerMessageId",
                ],
            );
            if let Some(id) = message_id {
                return Some(format!("message:{id}"));
            }
            if let Some(turn_id) = turn_id {
                return Some(format!("message:turn:{turn_id}:{seq}"));
            }
            Some(format!("message:seq:{seq}"))
        }
        "thought_chunk" => {
            let item_id = read_payload_string(payload, &["item_id", "itemId"]);
            let summary_index =
                read_payload_i64(payload, &["summary_index", "summaryIndex"]).unwrap_or(0);
            if let Some(id) = item_id {
                return Some(format!("thought:{id}:{summary_index}"));
            }
            if let Some(turn_id) = turn_id {
                return Some(format!("thought:turn:{turn_id}:{summary_index}"));
            }
            Some(format!("thought:seq:{seq}:{summary_index}"))
        }
        "notice" => {
            let kind = read_payload_string(payload, &["kind"]);
            if kind.as_deref() == Some("reasoning_summary") {
                let item_id = read_payload_string(payload, &["item_id", "itemId"]);
                let summary_index =
                    read_payload_i64(payload, &["summary_index", "summaryIndex"]).unwrap_or(0);
                if let Some(id) = item_id {
                    return Some(format!("thought:{id}:{summary_index}"));
                }
                if let Some(turn_id) = turn_id {
                    return Some(format!("thought:turn:{turn_id}:{summary_index}"));
                }
                return Some(format!("thought:seq:{seq}:{summary_index}"));
            }
            if kind.as_deref() == Some("ask_user_question") {
                if let Some(tool_call_id) =
                    read_payload_string(payload, &["tool_call_id", "toolCallId"])
                {
                    return Some(format!("tool:{tool_call_id}"));
                }
            }
            None
        }
        "tool_call" | "tool_call_update" | "tool_result" => {
            let tool_call_id = read_payload_string(payload, &["tool_call_id", "toolCallId"]);
            tool_call_id.map(|id| format!("tool:{id}"))
        }
        _ => None,
    }
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid datetime: {value}"))?
        .with_timezone(&Utc))
}

fn appearance_cmp(a: &Appearance, b: &Appearance) -> Ordering {
    let time_cmp = a.created_at.cmp(&b.created_at);
    if time_cmp != Ordering::Equal {
        return time_cmp;
    }
    match (a.seq, b.seq) {
        (Some(sa), Some(sb)) if sa != sb => return sa.cmp(&sb),
        (Some(_), None) => return Ordering::Less,
        (None, Some(_)) => return Ordering::Greater,
        _ => {}
    }
    match (a.turn_sequence, b.turn_sequence) {
        (Some(sa), Some(sb)) if sa != sb => return sa.cmp(&sb),
        (Some(_), None) => return Ordering::Less,
        (None, Some(_)) => return Ordering::Greater,
        _ => {}
    }
    a.key.cmp(&b.key)
}

fn default_data_root() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("CTX_DATA_ROOT") {
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".ctx"))
}

fn parse_args() -> Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    let mut data_root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--data-root" {
            if let Some(value) = args.next() {
                data_root = Some(PathBuf::from(value));
            }
        }
    }
    Ok(data_root.unwrap_or(default_data_root()?))
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_root = parse_args()?;
    let stores = StoreManager::open(&data_root)
        .await
        .with_context(|| format!("opening store manager at {}", data_root.display()))?;
    let workspaces = stores.global().list_workspaces().await?;
    if workspaces.is_empty() {
        println!("no workspaces found under {}", data_root.display());
        return Ok(());
    }

    for workspace in workspaces {
        let store = stores.workspace(workspace.id).await?;
        let session_rows = sqlx::query("SELECT id FROM sessions")
            .fetch_all(store.pool())
            .await?;
        if session_rows.is_empty() {
            continue;
        }
        println!(
            "backfilling workspace {} ({} sessions)",
            workspace.id.0,
            session_rows.len()
        );
        for row in session_rows {
            let session_id: String = row.try_get("id")?;
            backfill_session(store.pool(), &session_id)
                .await
                .with_context(|| format!("backfilling session {session_id}"))?;
        }
    }

    Ok(())
}

async fn backfill_session(pool: &sqlx::Pool<sqlx::Sqlite>, session_id: &str) -> Result<()> {
    let mut conn = pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;

    let message_rows = sqlx::query(
        r#"SELECT id, created_at, turn_sequence, order_seq
           FROM messages
           WHERE session_id = ?
           ORDER BY created_at ASC, turn_sequence ASC"#,
    )
    .bind(session_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut messages = Vec::with_capacity(message_rows.len());
    for row in message_rows {
        let id: String = row.try_get("id")?;
        let created_at: String = row.try_get("created_at")?;
        let turn_sequence: Option<i64> = row.try_get("turn_sequence")?;
        let order_seq: Option<i64> = row.try_get("order_seq")?;
        messages.push(MessageRow {
            id,
            created_at: parse_datetime(&created_at)?,
            turn_sequence,
            order_seq,
        });
    }

    let event_rows = sqlx::query(
        r#"SELECT id, seq, turn_id, event_type, payload_json, created_at
           FROM session_events
           WHERE session_id = ? AND transient = 0
           ORDER BY seq ASC"#,
    )
    .bind(session_id)
    .fetch_all(&mut *conn)
    .await?;

    let mut events = Vec::with_capacity(event_rows.len());
    for row in event_rows {
        let id: String = row.try_get("id")?;
        let seq: i64 = row.try_get("seq")?;
        let turn_id: Option<String> = row.try_get("turn_id")?;
        let event_type: String = row.try_get("event_type")?;
        let payload_json: String = row.try_get("payload_json")?;
        let created_at: String = row.try_get("created_at")?;
        let payload = serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::Null);
        events.push(EventRow {
            id,
            seq,
            turn_id,
            event_type,
            payload,
            created_at: parse_datetime(&created_at)?,
        });
    }

    let mut order_seq_by_key: HashMap<String, i64> = HashMap::new();
    let mut max_existing: i64 = 0;

    for msg in &messages {
        if let Some(order_seq) = msg.order_seq {
            let key = format!("message:{}", msg.id);
            order_seq_by_key.entry(key).or_insert(order_seq);
            max_existing = max_existing.max(order_seq);
        }
    }

    for ev in &events {
        let key = match build_item_key(&ev.event_type, &ev.payload, ev.turn_id.as_deref(), ev.seq) {
            Some(key) => key,
            None => continue,
        };
        if let Some(order_seq) = read_order_seq(&ev.payload) {
            let entry = order_seq_by_key.entry(key).or_insert(order_seq);
            max_existing = max_existing.max(*entry);
        }
    }

    let mut appearances: Vec<Appearance> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();
    for ev in &events {
        let key = match build_item_key(&ev.event_type, &ev.payload, ev.turn_id.as_deref(), ev.seq) {
            Some(key) => key,
            None => continue,
        };
        if order_seq_by_key.contains_key(&key) {
            continue;
        }
        if seen_keys.insert(key.clone()) {
            appearances.push(Appearance {
                key,
                created_at: ev.created_at,
                seq: Some(ev.seq),
                turn_sequence: None,
            });
        }
    }
    for msg in &messages {
        let key = format!("message:{}", msg.id);
        if order_seq_by_key.contains_key(&key) {
            continue;
        }
        appearances.push(Appearance {
            key,
            created_at: msg.created_at,
            seq: None,
            turn_sequence: msg.turn_sequence,
        });
    }

    appearances.sort_by(appearance_cmp);

    let mut next_seq = max_existing.saturating_add(1);
    for appearance in appearances {
        if order_seq_by_key.contains_key(&appearance.key) {
            continue;
        }
        order_seq_by_key.insert(appearance.key, next_seq);
        next_seq = next_seq.saturating_add(1);
    }

    for ev in &events {
        let key = match build_item_key(&ev.event_type, &ev.payload, ev.turn_id.as_deref(), ev.seq) {
            Some(key) => key,
            None => continue,
        };
        let order_seq = match order_seq_by_key.get(&key) {
            Some(value) => *value,
            None => continue,
        };
        let mut payload = ev.payload.clone();
        let existing = read_order_seq(&payload);
        if existing == Some(order_seq) {
            continue;
        }
        if payload.is_object() {
            insert_order_seq(&mut payload, order_seq);
            let payload_json = serde_json::to_string(&payload)?;
            sqlx::query("UPDATE session_events SET payload_json = ? WHERE id = ?")
                .bind(payload_json)
                .bind(&ev.id)
                .execute(&mut *conn)
                .await?;
        }
    }

    for msg in &messages {
        if msg.order_seq.is_some() {
            continue;
        }
        let key = format!("message:{}", msg.id);
        let order_seq = match order_seq_by_key.get(&key) {
            Some(value) => *value,
            None => continue,
        };
        sqlx::query("UPDATE messages SET order_seq = ? WHERE id = ?")
            .bind(order_seq)
            .bind(&msg.id)
            .execute(&mut *conn)
            .await?;
    }

    for (key, order_seq) in &order_seq_by_key {
        let Some(tool_call_id) = key.strip_prefix("tool:") else {
            continue;
        };
        sqlx::query(
            r#"UPDATE session_turn_tools
               SET order_seq = ?
               WHERE session_id = ?
                 AND tool_call_id = ?
                 AND (order_seq IS NULL OR order_seq != ?)"#,
        )
        .bind(order_seq)
        .bind(session_id)
        .bind(tool_call_id)
        .bind(order_seq)
        .execute(&mut *conn)
        .await?;
    }

    sqlx::query("COMMIT").execute(&mut *conn).await?;
    Ok(())
}

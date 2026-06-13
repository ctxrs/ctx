use ctx_core::ids::{MessageId, RunId, SessionId, TaskId, TurnId};
use ctx_store::Store;
use sqlx::{QueryBuilder, Sqlite};

pub struct CtxUiSizedHeadSeedSpec {
    pub turn_count: i64,
    pub message_count: i64,
    pub tool_count: i64,
    pub event_count: i64,
    pub tool_output_bytes: usize,
}

pub struct CtxUiSizedHeadSeedStats {
    pub event_count: i64,
    pub tool_count: i64,
    pub message_count: i64,
}

pub struct CtxUiSizedToolSummaryProbe {
    pub latest_turn_id: TurnId,
    pub bounded_tool_count: usize,
    pub oldest_loaded_order_seq: i64,
}

struct CtxUiTurnSeed {
    index: i64,
    run_id: String,
    turn_id: String,
    started_at: String,
    start_seq: i64,
    end_seq: Option<i64>,
    status: &'static str,
    tool_total: i64,
}

const CTX_UI_SIZED_SEED_BATCH: i64 = 500;

pub(super) fn fixed_test_utc(offset_seconds: i64) -> chrono::DateTime<chrono::Utc> {
    let base = chrono::DateTime::from_timestamp(1735689600, 0)
        .expect("fixed test timestamp should be valid");
    base + chrono::Duration::seconds(offset_seconds)
}

fn build_ctx_ui_sized_turns(seed: &CtxUiSizedHeadSeedSpec) -> Vec<CtxUiTurnSeed> {
    let started_at = chrono::Utc::now();
    let tool_turn_start = (seed.turn_count - 60).max(0);
    let tools_per_turn = seed.tool_count / 60;
    let tool_remainder = seed.tool_count % 60;

    (0..seed.turn_count)
        .map(|index| {
            let start_seq = 1 + (index * seed.event_count / seed.turn_count);
            let tool_total = if index < tool_turn_start {
                0
            } else {
                let offset = index - tool_turn_start;
                tools_per_turn + if offset < tool_remainder { 1 } else { 0 }
            };
            CtxUiTurnSeed {
                index,
                run_id: RunId::new().0.to_string(),
                turn_id: TurnId::new().0.to_string(),
                started_at: (started_at + chrono::Duration::milliseconds(index)).to_rfc3339(),
                start_seq,
                end_seq: if index + 1 == seed.turn_count {
                    None
                } else {
                    Some(start_seq + 1)
                },
                status: if index + 1 == seed.turn_count {
                    "running"
                } else {
                    "completed"
                },
                tool_total,
            }
        })
        .collect()
}

async fn insert_ctx_ui_sized_turns(
    store: &Store,
    session_id: &str,
    turns: &[CtxUiTurnSeed],
) -> anyhow::Result<()> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        r#"INSERT INTO session_turns (
            turn_id, session_id, run_id, user_message_id, status, start_seq, end_seq,
            started_at, updated_at, assistant_partial, thought_partial, metrics_json,
            tool_total, tool_pending, tool_running, tool_completed, tool_failed
        ) "#,
    );
    builder.push_values(turns, |mut values, row| {
        values
            .push_bind(&row.turn_id)
            .push_bind(session_id)
            .push_bind(&row.run_id)
            .push_bind(Option::<String>::None)
            .push_bind(row.status)
            .push_bind(row.start_seq)
            .push_bind(row.end_seq)
            .push_bind(&row.started_at)
            .push_bind(&row.started_at)
            .push_bind(Option::<String>::None)
            .push_bind(Option::<String>::None)
            .push_bind(Option::<String>::None)
            .push_bind(row.tool_total)
            .push_bind(0_i64)
            .push_bind(if row.status == "running" {
                1_i64
            } else {
                0_i64
            })
            .push_bind(if row.status == "running" {
                0_i64
            } else {
                row.tool_total
            })
            .push_bind(0_i64);
    });
    builder.build().execute(store.pool()).await?;
    Ok(())
}

async fn seed_ctx_ui_sized_events(
    store: &Store,
    session_id: &str,
    seed: &CtxUiSizedHeadSeedSpec,
    turns: &[CtxUiTurnSeed],
) -> anyhow::Result<()> {
    let mut event_seq = 1_i64;
    while event_seq <= seed.event_count {
        let end = (event_seq + CTX_UI_SIZED_SEED_BATCH - 1).min(seed.event_count);
        let rows = (event_seq..=end).collect::<Vec<_>>();
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO session_events (
                seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
            ) "#,
        );
        builder.push_values(&rows, |mut values, seq| {
            let turn_index =
                ((*seq - 1) * seed.turn_count / seed.event_count).clamp(0, seed.turn_count - 1);
            let turn = &turns[turn_index as usize];
            let event_type = match *seq % 11 {
                0 => "tool_call",
                1 => "tool_result",
                2 => "assistant_message_inserted",
                3 => "assistant_complete",
                _ => "notice",
            };
            let payload = ctx_ui_sized_event_payload(seed, turn, *seq, event_type);
            values
                .push_bind(*seq)
                .push_bind(uuid::Uuid::new_v4().to_string())
                .push_bind(session_id)
                .push_bind(&turn.run_id)
                .push_bind(&turn.turn_id)
                .push_bind(event_type)
                .push_bind(payload.to_string())
                .push_bind(if *seq % 17 == 0 { 1_i64 } else { 0_i64 })
                .push_bind(&turn.started_at);
        });
        builder.build().execute(store.pool()).await?;
        event_seq = end + 1;
    }
    Ok(())
}

fn ctx_ui_sized_event_payload(
    seed: &CtxUiSizedHeadSeedSpec,
    turn: &CtxUiTurnSeed,
    seq: i64,
    event_type: &str,
) -> serde_json::Value {
    if turn.index + 1 == seed.turn_count && matches!(event_type, "tool_call" | "tool_result") {
        return serde_json::json!({
            "kind": "ctx_ui_sized_fixture",
            "seq": seq,
            "turn_index": turn.index,
            "tool_call_id": format!("ctx-ui-live-tool-{seq}"),
            "order_seq": seed.tool_count + seq,
            "title": format!("Live fixture command {seq}"),
            "status": if event_type == "tool_result" { "completed" } else { "pending" },
            "rawInput": {
                "cmd": "printf ctx-ui-live-fixture",
                "seq": seq,
            },
            "output_text": format!("live fixture output {seq}"),
        });
    }

    serde_json::json!({
        "kind": "ctx_ui_sized_fixture",
        "seq": seq,
        "turn_index": turn.index,
    })
}

async fn seed_ctx_ui_sized_messages(
    store: &Store,
    session_id: &str,
    task_id: &str,
    seed: &CtxUiSizedHeadSeedSpec,
    turns: &[CtxUiTurnSeed],
) -> anyhow::Result<()> {
    let mut message_index = 0_i64;
    while message_index < seed.message_count {
        let end = (message_index + CTX_UI_SIZED_SEED_BATCH).min(seed.message_count);
        let rows = (message_index..end).collect::<Vec<_>>();
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO messages (
                id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content,
                attachments_json, delivery, delivered_at, created_at
            ) "#,
        );
        builder.push_values(&rows, |mut values, index| {
            let turn_index =
                (*index * seed.turn_count / seed.message_count).clamp(0, seed.turn_count - 1);
            let turn = &turns[turn_index as usize];
            values
                .push_bind(MessageId::new().0.to_string())
                .push_bind(session_id)
                .push_bind(task_id)
                .push_bind(&turn.run_id)
                .push_bind(&turn.turn_id)
                .push_bind(*index)
                .push_bind(*index)
                .push_bind("assistant")
                .push_bind(format!("ctx-ui fixture answer {index}"))
                .push_bind("[]")
                .push_bind("immediate")
                .push_bind(Option::<String>::None)
                .push_bind(&turn.started_at);
        });
        builder.build().execute(store.pool()).await?;
        message_index = end;
    }
    Ok(())
}

async fn seed_ctx_ui_sized_tools(
    store: &Store,
    session_id: &str,
    seed: &CtxUiSizedHeadSeedSpec,
    turns: &[CtxUiTurnSeed],
) -> anyhow::Result<()> {
    let output_text = "x".repeat(seed.tool_output_bytes);
    let input_json = serde_json::json!({
        "cmd": "printf ctx-ui-sized-fixture",
        "env": {"CTX_FIXTURE": "long-tail"},
        "payload": "y".repeat(256),
    })
    .to_string();
    let tool_turns = turns
        .iter()
        .filter(|turn| turn.tool_total > 0)
        .collect::<Vec<_>>();

    let mut tool_index = 0_i64;
    while tool_index < seed.tool_count {
        let end = (tool_index + CTX_UI_SIZED_SEED_BATCH).min(seed.tool_count);
        let rows = (tool_index..end).collect::<Vec<_>>();
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO session_turn_tools (
                session_id, tool_call_id, turn_id, tool_kind, provider_tool_name, title, subtitle,
                status, input_json, output_text, order_seq, first_event_seq, input_truncated,
                input_original_bytes, output_truncated, output_original_bytes, created_at, updated_at
            ) "#,
        );
        builder.push_values(&rows, |mut values, index| {
            let mut turn = tool_turns[(*index as usize) % tool_turns.len()];
            if turn.index + 1 == seed.turn_count && tool_turns.len() > 1 {
                turn = tool_turns[tool_turns.len() - 2];
            }
            values
                .push_bind(session_id)
                .push_bind(format!("ctx-ui-tool-{index}"))
                .push_bind(&turn.turn_id)
                .push_bind("exec")
                .push_bind("exec_command")
                .push_bind(format!("Fixture command {index}"))
                .push_bind(format!("turn {}", turn.index))
                .push_bind("completed")
                .push_bind(&input_json)
                .push_bind(&output_text)
                .push_bind(*index)
                .push_bind(turn.start_seq)
                .push_bind(0_i64)
                .push_bind(input_json.len() as i64)
                .push_bind(0_i64)
                .push_bind(output_text.len() as i64)
                .push_bind(&turn.started_at)
                .push_bind(&turn.started_at);
        });
        builder.build().execute(store.pool()).await?;
        tool_index = end;
    }
    Ok(())
}

pub(super) async fn seed_ctx_ui_sized_session(
    store: &Store,
    session_id: SessionId,
    task_id: TaskId,
    seed: &CtxUiSizedHeadSeedSpec,
) -> anyhow::Result<()> {
    let session_id = session_id.0.to_string();
    let task_id = task_id.0.to_string();
    let turns = build_ctx_ui_sized_turns(seed);

    insert_ctx_ui_sized_turns(store, &session_id, &turns).await?;
    seed_ctx_ui_sized_events(store, &session_id, seed, &turns).await?;
    seed_ctx_ui_sized_messages(store, &session_id, &task_id, seed, &turns).await?;
    seed_ctx_ui_sized_tools(store, &session_id, seed, &turns).await?;
    Ok(())
}

pub(super) async fn latest_ctx_ui_sized_turn_id(
    store: &Store,
    session_id: SessionId,
) -> anyhow::Result<TurnId> {
    let value: String = sqlx::query_scalar(
        r#"SELECT turn_id
           FROM session_turns
           WHERE session_id = ?
           ORDER BY start_seq DESC
           LIMIT 1"#,
    )
    .bind(session_id.0.to_string())
    .fetch_one(store.pool())
    .await?;
    let parsed = uuid::Uuid::parse_str(&value)?;
    Ok(TurnId(parsed))
}

pub(super) async fn tail_ctx_ui_sized_turn_ids(
    store: &Store,
    session_id: SessionId,
    limit: i64,
) -> anyhow::Result<Vec<TurnId>> {
    let rows: Vec<String> = sqlx::query_scalar(
        r#"SELECT turn_id
           FROM session_turns
           WHERE session_id = ?
           ORDER BY start_seq DESC
           LIMIT ?"#,
    )
    .bind(session_id.0.to_string())
    .bind(limit)
    .fetch_all(store.pool())
    .await?;
    rows.into_iter()
        .map(|value| {
            uuid::Uuid::parse_str(&value)
                .map(TurnId)
                .map_err(Into::into)
        })
        .collect()
}

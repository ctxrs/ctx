use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{Message, MessageDelivery, MessageRole, SessionEventType, VcsKind};
use ctx_store::Store;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::Row;
use tokio::process::Command;
use tokio::time::sleep;

const LOCK_CHILD_ENV: &str = "CTX_STORE_LOCK_CHILD";
const LOCK_DB_PATH_ENV: &str = "CTX_STORE_LOCK_DB_PATH";
const LOCK_READY_PATH_ENV: &str = "CTX_STORE_LOCK_READY_PATH";
const LOCK_RELEASE_PATH_ENV: &str = "CTX_STORE_LOCK_RELEASE_PATH";
const STABLE_0_60_LAST_MIGRATION_VERSION: i64 = 60;
const LEGACY_DROP_WORKSPACE_MESSAGE_INDEX_SQL: &str = "\
DROP INDEX IF EXISTS workspace_message_index_workspace_id_idx;\n\
DROP TABLE IF EXISTS workspace_message_index;\n";

#[test]
fn migration_versions_are_unique() -> Result<()> {
    let migrations = migration_files()?;
    let mut seen = std::collections::BTreeMap::new();

    for migration in migrations {
        let version = migration_version(&migration)?;
        if let Some(previous) = seen.insert(version, migration.clone()) {
            anyhow::bail!(
                "duplicate migration version {version}: {} and {}",
                previous.display(),
                migration.display()
            );
        }
    }

    Ok(())
}

#[tokio::test]
async fn session_head_lookup_indexes_exist_and_match_query_shapes() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir for index hardening")?;
    let db_path = tempdir.path().join("db.sqlite");
    let store = Store::open(&db_path)
        .await
        .context("opening store for index hardening")?;
    let pool = store.pool();

    assert_index_columns(
        pool,
        "idx_messages_session_turn_created",
        &["session_id", "turn_id", "created_at", "turn_sequence"],
    )
    .await?;
    assert_index_columns(
        pool,
        "idx_session_turn_tools_session_turn_created",
        &["session_id", "turn_id", "created_at"],
    )
    .await?;

    assert_query_plan_uses_index(
        pool,
        "messages head lookup",
        "idx_messages_session_turn_created",
        r#"EXPLAIN QUERY PLAN
           SELECT id
           FROM messages
           WHERE session_id = ? AND turn_id = ?
           ORDER BY created_at ASC, turn_sequence ASC"#,
    )
    .await?;
    assert_query_plan_uses_index(
        pool,
        "turn tool head lookup",
        "idx_session_turn_tools_session_turn_created",
        r#"EXPLAIN QUERY PLAN
           SELECT tool_call_id
           FROM session_turn_tools
           WHERE session_id = ? AND turn_id = ?
           ORDER BY created_at ASC"#,
    )
    .await?;

    store.close().await;
    Ok(())
}

#[tokio::test]
async fn migrations_upgrade_cleanly_from_every_historical_prefix() -> Result<()> {
    let migrations = migration_files()?;
    assert!(
        migrations.len() > 3,
        "expected enough historical migrations to exercise upgrade prefixes"
    );

    for prefix_len in 1..migrations.len() {
        let tempdir = tempfile::tempdir().context("creating tempdir for migration prefix")?;
        let subset_dir = tempdir.path().join("subset-migrations");
        fs::create_dir_all(&subset_dir).context("creating subset migration dir")?;
        for migration in &migrations[..prefix_len] {
            let filename = migration
                .file_name()
                .context("migration file missing name")?;
            fs::copy(migration, subset_dir.join(filename)).with_context(|| {
                format!(
                    "copying {} into subset prefix {}",
                    migration.display(),
                    prefix_len
                )
            })?;
        }

        let db_path = tempdir.path().join("db.sqlite");
        fs::File::create(&db_path).context("creating sqlite file")?;
        let sqlite_url = sqlite_url(&db_path);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&sqlite_url)
            .await
            .with_context(|| {
                format!("connecting partial-migration pool for prefix {prefix_len}")
            })?;
        let subset_migrator = Migrator::new(subset_dir.clone())
            .await
            .with_context(|| format!("loading subset migrator for prefix {prefix_len}"))?;
        subset_migrator
            .run(&pool)
            .await
            .with_context(|| format!("running subset migrator for prefix {prefix_len}"))?;
        pool.close().await;

        let store = Store::open(&db_path)
            .await
            .with_context(|| format!("opening store from prefix {prefix_len}"))?;
        store
            .create_workspace(
                format!("upgrade-{prefix_len}"),
                format!("/tmp/upgrade-{prefix_len}"),
                VcsKind::Git,
            )
            .await
            .with_context(|| format!("creating workspace after upgrading prefix {prefix_len}"))?;
        store.close().await;

        assert_store_integrity(&db_path).await?;
        assert_eq!(
            applied_migration_count(&db_path).await?,
            migrations.len() as i64,
            "expected all migrations applied after upgrading prefix {prefix_len}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn open_upgrades_from_stable_0_60_migration_prefix() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir for 0.60 upgrade")?;
    let subset_dir = tempdir.path().join("stable-0-60-migrations");
    fs::create_dir_all(&subset_dir).context("creating 0.60 migration dir")?;

    let migrations = migration_files()?;
    for migration in &migrations {
        if migration_version(migration)? <= STABLE_0_60_LAST_MIGRATION_VERSION {
            let filename = migration
                .file_name()
                .context("migration file missing name")?;
            fs::copy(migration, subset_dir.join(filename))
                .with_context(|| format!("copying {} into 0.60 subset", migration.display()))?;
        }
    }

    let db_path = tempdir.path().join("db.sqlite");
    fs::File::create(&db_path).context("creating sqlite file")?;
    let sqlite_url = sqlite_url(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .context("connecting 0.60 prefix pool")?;
    let subset_migrator = Migrator::new(subset_dir.clone())
        .await
        .context("loading 0.60 subset migrator")?;
    subset_migrator
        .run(&pool)
        .await
        .context("running 0.60 subset migrator")?;
    pool.close().await;

    let store = Store::open(&db_path)
        .await
        .context("opening store from stable 0.60 migration prefix")?;
    store
        .create_workspace(
            "stable-0-60-upgrade".into(),
            "/tmp/stable-0-60".into(),
            VcsKind::Git,
        )
        .await
        .context("creating workspace after 0.60 upgrade")?;
    store.close().await;

    assert_store_integrity(&db_path).await?;
    assert_eq!(
        applied_migration_count(&db_path).await?,
        migrations.len() as i64,
        "expected all migrations after 0.60 upgrade"
    );
    assert!(column_exists(&db_path, "sessions", "archived_at").await?);
    assert!(index_exists(&db_path, "idx_sessions_parent_relationship_active").await?);

    Ok(())
}

#[tokio::test]
async fn open_repairs_partially_applied_duplicate_tool_display_migration() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir for duplicate migration repair")?;
    let subset_dir = tempdir.path().join("subset-migrations");
    fs::create_dir_all(&subset_dir).context("creating subset migration dir")?;

    let migrations = migration_files()?;
    let tool_display_migration = migrations
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "0047_tool_display_fields.sql")
        })
        .cloned()
        .context("finding tool display migration")?;

    for migration in &migrations {
        if migration_version(migration)? <= 45 {
            let filename = migration
                .file_name()
                .context("migration file missing name")?;
            fs::copy(migration, subset_dir.join(filename)).with_context(|| {
                format!(
                    "copying {} into duplicate-version subset",
                    migration.display()
                )
            })?;
        }
    }

    fs::copy(
        &tool_display_migration,
        subset_dir.join("0046_tool_display_fields.sql"),
    )
    .with_context(|| {
        format!(
            "copying {} into duplicate-version subset as 0046",
            tool_display_migration.display()
        )
    })?;

    let db_path = tempdir.path().join("db.sqlite");
    fs::File::create(&db_path).context("creating sqlite file")?;
    let sqlite_url = sqlite_url(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .context("connecting partial-migration pool for duplicate repair")?;
    let subset_migrator = Migrator::new(subset_dir.clone())
        .await
        .context("loading subset migrator for duplicate repair")?;
    subset_migrator
        .run(&pool)
        .await
        .context("running subset migrator for duplicate repair")?;
    pool.close().await;

    let store = Store::open(&db_path)
        .await
        .context("opening store after duplicate-version partial migration")?;
    store.close().await;

    assert_store_integrity(&db_path).await?;
    assert!(column_exists(&db_path, "sessions", "reasoning_effort").await?);
    assert!(column_exists(&db_path, "session_turn_tools", "provider_tool_name").await?);
    assert!(column_exists(&db_path, "session_turn_tools", "subtitle").await?);

    let applied = applied_migrations(&db_path).await?;
    assert!(applied
        .iter()
        .any(|(version, description)| *version == 46 && description == "session reasoning effort"));
    assert!(applied
        .iter()
        .any(|(version, description)| *version == 47 && description == "tool display fields"));

    Ok(())
}

#[tokio::test]
async fn open_repairs_workspace_message_index_migration_version_conflict() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir for workspace message repair")?;
    let subset_dir = tempdir.path().join("subset-migrations");
    fs::create_dir_all(&subset_dir).context("creating subset migration dir")?;

    let migrations = migration_files()?;
    for migration in &migrations {
        let version = migration_version(migration)?;
        if version <= 48
            || migration
                .file_name()
                .is_some_and(|name| name == "0050_drop_workspace_owned_routing_indexes.sql")
        {
            let filename = migration
                .file_name()
                .context("migration file missing name")?;
            fs::copy(migration, subset_dir.join(filename)).with_context(|| {
                format!(
                    "copying {} into workspace-message repair subset",
                    migration.display()
                )
            })?;
        }
    }

    fs::write(
        subset_dir.join("0049_drop_workspace_message_index.sql"),
        LEGACY_DROP_WORKSPACE_MESSAGE_INDEX_SQL,
    )
    .context("writing legacy workspace message index migration")?;

    let db_path = tempdir.path().join("db.sqlite");
    fs::File::create(&db_path).context("creating sqlite file")?;
    let sqlite_url = sqlite_url(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .context("connecting partial-migration pool for workspace message repair")?;
    let subset_migrator = Migrator::new(subset_dir.clone())
        .await
        .context("loading subset migrator for workspace message repair")?;
    subset_migrator
        .run(&pool)
        .await
        .context("running subset migrator for workspace message repair")?;
    pool.close().await;

    let store = Store::open(&db_path)
        .await
        .context("opening store after workspace message version repair")?;
    store.close().await;

    assert_store_integrity(&db_path).await?;
    assert!(column_exists(&db_path, "session_turn_tools", "order_seq").await?);

    let applied = applied_migrations(&db_path).await?;
    assert!(applied
        .iter()
        .any(|(version, description)| *version == 55 && description == "tool order seq"));
    assert!(applied.iter().any(|(version, description)| {
        *version == 50 && description == "drop workspace owned routing indexes"
    }));
    assert!(applied.iter().any(|(version, description)| {
        *version == 51 && description == "drop workspace message index"
    }));

    Ok(())
}

#[tokio::test]
async fn unknown_data_cleanup_migration_deletes_noisy_notices() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir for cleanup migration")?;
    let db_path = tempdir.path().join("db.sqlite");
    fs::File::create(&db_path).context("creating sqlite file")?;
    let sqlite_url = sqlite_url(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .context("connecting cleanup migration pool")?;

    execute_sql_script(
        &pool,
        r#"
        CREATE TABLE session_events (
            id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL,
            event_type TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            transient INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE session_head_materializations (
            session_id TEXT PRIMARY KEY NOT NULL
        );
        CREATE TABLE session_active_snapshot_heads (
            session_id TEXT PRIMARY KEY NOT NULL
        );
        CREATE TABLE session_snapshot_summaries (
            session_id TEXT PRIMARY KEY NOT NULL,
            projection_rev INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )
    .await?;

    insert_cleanup_event(
        &pool,
        "delete-tool-delta",
        "session-delete",
        "tool.output_delta",
        Some("data"),
        1,
    )
    .await?;
    insert_cleanup_event(
        &pool,
        "keep-no-channel-delta",
        "session-keep-no-channel",
        "message_delta",
        None,
        1,
    )
    .await?;
    insert_cleanup_event(
        &pool,
        "delete-progress",
        "session-delete",
        "tool.progress",
        Some("data"),
        1,
    )
    .await?;
    insert_cleanup_event(
        &pool,
        "keep-control-delta",
        "session-keep",
        "tool.output_delta",
        Some("control"),
        0,
    )
    .await?;
    insert_cleanup_event(
        &pool,
        "keep-nontransient-data",
        "session-data-channel",
        "tool.output-delta",
        Some("data"),
        0,
    )
    .await?;

    for session_id in [
        "session-delete",
        "session-keep",
        "session-data-channel",
        "session-keep-no-channel",
    ] {
        sqlx::query("INSERT INTO session_head_materializations (session_id) VALUES (?)")
            .bind(session_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO session_active_snapshot_heads (session_id) VALUES (?)")
            .bind(session_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO session_snapshot_summaries (session_id, projection_rev, updated_at) VALUES (?, 7, 'before')",
        )
        .bind(session_id)
        .execute(&pool)
        .await?;
    }

    execute_sql_script(
        &pool,
        include_str!("../migrations/0068_cleanup_unknown_data_events.sql"),
    )
    .await?;

    let remaining_events: Vec<String> =
        sqlx::query_scalar("SELECT id FROM session_events ORDER BY id")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        remaining_events,
        vec![
            "keep-control-delta",
            "keep-no-channel-delta",
            "keep-nontransient-data",
        ]
    );

    let remaining_materializations: Vec<String> = sqlx::query_scalar(
        "SELECT session_id FROM session_head_materializations ORDER BY session_id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        remaining_materializations,
        vec![
            "session-data-channel",
            "session-keep",
            "session-keep-no-channel",
        ]
    );

    let remaining_active_heads: Vec<String> = sqlx::query_scalar(
        "SELECT session_id FROM session_active_snapshot_heads ORDER BY session_id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        remaining_active_heads,
        vec![
            "session-data-channel",
            "session-keep",
            "session-keep-no-channel",
        ]
    );

    let projection_revs: Vec<(String, i64)> = sqlx::query(
        "SELECT session_id, projection_rev FROM session_snapshot_summaries ORDER BY session_id",
    )
    .fetch_all(&pool)
    .await?
    .into_iter()
    .map(|row| Ok((row.try_get("session_id")?, row.try_get("projection_rev")?)))
    .collect::<Result<_>>()?;
    assert_eq!(
        projection_revs,
        vec![
            ("session-data-channel".to_string(), 7),
            ("session-delete".to_string(), 8),
            ("session-keep".to_string(), 7),
            ("session-keep-no-channel".to_string(), 7),
        ]
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn turn_error_migration_backfills_failure_and_removes_error_events() -> Result<()> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;

    execute_sql_script(
        &pool,
        r#"
        CREATE TABLE session_events (
            seq INTEGER PRIMARY KEY,
            id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            run_id TEXT,
            turn_id TEXT,
            event_type TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            transient INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE session_turns (
            session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            status TEXT NOT NULL,
            end_seq INTEGER,
            updated_at TEXT NOT NULL,
            failure_json TEXT
        );
        CREATE TABLE session_head_materializations (
            session_id TEXT NOT NULL
        );
        CREATE TABLE session_active_snapshot_heads (
            session_id TEXT NOT NULL
        );
        CREATE TABLE session_snapshot_summaries (
            session_id TEXT NOT NULL,
            projection_rev INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )
    .await?;

    for (session_id, turn_id, status) in [
        ("s1", "t1", "failed"),
        ("s2", "t2", "running"),
        ("s3", "t3", "running"),
        ("s4", "t4", "completed"),
        ("s5", "t5", "completed"),
        ("s6", "t6", "running"),
        ("s7", "t7", "completed"),
        ("s8", "t8", "running"),
    ] {
        sqlx::query(
            "INSERT INTO session_turns (session_id, turn_id, status, updated_at) VALUES (?, ?, ?, 'before')",
        )
        .bind(session_id)
        .bind(turn_id)
        .bind(status)
        .execute(&pool)
        .await?;
        sqlx::query("INSERT INTO session_head_materializations (session_id) VALUES (?)")
            .bind(session_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO session_active_snapshot_heads (session_id) VALUES (?)")
            .bind(session_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO session_snapshot_summaries (session_id, projection_rev, updated_at) VALUES (?, 7, 'before')",
        )
        .bind(session_id)
        .execute(&pool)
        .await?;
    }

    sqlx::query(
        r#"INSERT INTO session_events
           (seq, id, session_id, turn_id, event_type, payload_json, transient, created_at)
           VALUES
           (1, 'tf-duplicate', 's1', 't1', 'turn_finished', '{"status":"failed"}', 0, '2026-05-08T17:00:01Z'),
           (2, 'err-duplicate', 's1', 't1', 'error', '{"message":"provider failed","kind":"provider_protocol_violation","details":{"exit_code":1}}', 0, '2026-05-08T17:00:02Z'),
           (3, 'err-only', 's2', 't2', 'error', '{"error":"startup timed out","reason":"provider_startup_timeout"}', 0, '2026-05-08T17:00:03Z'),
           (4, 'session-error', 's3', NULL, 'error', '{"message":"session level"}', 0, '2026-05-08T17:00:04Z'),
           (5, 'tf-no-error', 's4', 't4', 'turn_finished', '{"status":"failed","message":"failed from turn finished"}', 0, '2026-05-08T17:00:05Z'),
           (6, 'tf-earlier-failed', 's5', 't5', 'turn_finished', '{"status":"failed","message":"older failure"}', 0, '2026-05-08T17:00:06Z'),
           (7, 'tf-later-completed', 's5', 't5', 'turn_finished', '{"status":"completed"}', 0, '2026-05-08T17:00:07Z'),
           (8, 'tf-status-error', 's6', 't6', 'turn_finished', '{"status":"error","error":"status alias failure"}', 0, '2026-05-08T17:00:08Z'),
           (9, 'tf-before-error', 's7', 't7', 'turn_finished', '{"status":"completed"}', 0, '2026-05-08T17:00:09Z'),
           (10, 'err-after-completed', 's7', 't7', 'error', '{"message":"late terminal failure"}', 0, '2026-05-08T17:00:10Z'),
           (11, 'err-non-string-fields', 's8', 't8', 'error', '{"message":{"unexpected":true},"error":"string fallback","kind":{"name":"bad"},"reason":17,"provider":{"id":"bad"},"providerId":99,"details":{"raw":true}}', 0, '2026-05-08T17:00:11Z')"#,
    )
    .execute(&pool)
    .await?;

    execute_sql_script(
        &pool,
        include_str!("../migrations/0074_migrate_turn_errors_to_failure_projection.sql"),
    )
    .await?;

    let error_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_events WHERE event_type = 'error'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(error_count, 0);

    let s1_late_event_type: String =
        sqlx::query_scalar("SELECT event_type FROM session_events WHERE id = 'err-duplicate'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s1_late_event_type, "turn_finished");
    let s1_late_message: String = sqlx::query_scalar(
        "SELECT json_extract(payload_json, '$.message') FROM session_events WHERE id = 'err-duplicate'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s1_late_message, "provider failed");

    let s1_failure_message: String = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.message') FROM session_turns WHERE session_id = 's1'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s1_failure_message, "provider failed");
    let s1_end_seq: i64 =
        sqlx::query_scalar("SELECT end_seq FROM session_turns WHERE session_id = 's1'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s1_end_seq, 2);
    let s1_failure_detail: i64 = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.details.exit_code') FROM session_turns WHERE session_id = 's1'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s1_failure_detail, 1);

    let s2_event_type: String =
        sqlx::query_scalar("SELECT event_type FROM session_events WHERE id = 'err-only'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s2_event_type, "turn_finished");
    let s2_status: String =
        sqlx::query_scalar("SELECT status FROM session_turns WHERE session_id = 's2'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s2_status, "failed");
    let s2_end_seq: i64 =
        sqlx::query_scalar("SELECT end_seq FROM session_turns WHERE session_id = 's2'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s2_end_seq, 3);
    let s2_updated_at: String =
        sqlx::query_scalar("SELECT updated_at FROM session_turns WHERE session_id = 's2'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s2_updated_at, "2026-05-08T17:00:03Z");
    let s2_failure_message: String = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.message') FROM session_turns WHERE session_id = 's2'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s2_failure_message, "startup timed out");

    let s4_status: String =
        sqlx::query_scalar("SELECT status FROM session_turns WHERE session_id = 's4'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s4_status, "failed");
    let s4_failure_message: String = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.message') FROM session_turns WHERE session_id = 's4'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s4_failure_message, "failed from turn finished");

    let s5_status: String =
        sqlx::query_scalar("SELECT status FROM session_turns WHERE session_id = 's5'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s5_status, "completed");
    let s5_failure_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_turns WHERE session_id = 's5' AND failure_json IS NOT NULL",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s5_failure_count, 0);

    let s6_event_status: String = sqlx::query_scalar(
        "SELECT json_extract(payload_json, '$.status') FROM session_events WHERE id = 'tf-status-error'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s6_event_status, "failed");
    let s6_failure_message: String = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.message') FROM session_turns WHERE session_id = 's6'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s6_failure_message, "status alias failure");

    let s7_late_event_type: String = sqlx::query_scalar(
        "SELECT event_type FROM session_events WHERE id = 'err-after-completed'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s7_late_event_type, "turn_finished");
    let s7_status: String =
        sqlx::query_scalar("SELECT status FROM session_turns WHERE session_id = 's7'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(s7_status, "failed");
    let s7_failure_message: String = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.message') FROM session_turns WHERE session_id = 's7'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s7_failure_message, "late terminal failure");

    let s8_event_message: String = sqlx::query_scalar(
        "SELECT json_extract(payload_json, '$.message') FROM session_events WHERE id = 'err-non-string-fields'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s8_event_message, "string fallback");
    let s8_failure_message: String = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.message') FROM session_turns WHERE session_id = 's8'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s8_failure_message, "string fallback");
    let s8_failure_detail: bool = sqlx::query_scalar(
        "SELECT json_extract(failure_json, '$.details.raw') FROM session_turns WHERE session_id = 's8'",
    )
    .fetch_one(&pool)
    .await?;
    assert!(s8_failure_detail);
    let s8_typed_failure_field_count: i64 = sqlx::query_scalar(
        r#"
        SELECT
          CASE WHEN json_type(failure_json, '$.kind') IN ('null', 'text') THEN 0 ELSE 1 END +
          CASE WHEN json_type(failure_json, '$.reason') IN ('null', 'text') THEN 0 ELSE 1 END +
          CASE WHEN json_type(failure_json, '$.provider') IN ('null', 'text') THEN 0 ELSE 1 END +
          CASE WHEN json_type(failure_json, '$.provider_id') IN ('null', 'text') THEN 0 ELSE 1 END
        FROM session_turns
        WHERE session_id = 's8'
        "#,
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(s8_typed_failure_field_count, 0);

    let session_event_type: String =
        sqlx::query_scalar("SELECT event_type FROM session_events WHERE id = 'session-error'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(session_event_type, "notice");

    let materialized_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_head_materializations")
            .fetch_one(&pool)
            .await?;
    assert_eq!(materialized_count, 1);
    let remaining_materialized_session: String =
        sqlx::query_scalar("SELECT session_id FROM session_head_materializations")
            .fetch_one(&pool)
            .await?;
    assert_eq!(remaining_materialized_session, "s5");
    let active_head_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_active_snapshot_heads")
            .fetch_one(&pool)
            .await?;
    assert_eq!(active_head_count, 1);
    let remaining_active_head_session: String =
        sqlx::query_scalar("SELECT session_id FROM session_active_snapshot_heads")
            .fetch_one(&pool)
            .await?;
    assert_eq!(remaining_active_head_session, "s5");
    let projection_revs: Vec<(String, i64)> = sqlx::query(
        "SELECT session_id, projection_rev FROM session_snapshot_summaries ORDER BY session_id",
    )
    .fetch_all(&pool)
    .await?
    .into_iter()
    .map(|row| Ok((row.try_get("session_id")?, row.try_get("projection_rev")?)))
    .collect::<Result<_>>()?;
    assert_eq!(
        projection_revs,
        vec![
            ("s1".to_string(), 8),
            ("s2".to_string(), 8),
            ("s3".to_string(), 8),
            ("s4".to_string(), 8),
            ("s5".to_string(), 7),
            ("s6".to_string(), 8),
            ("s7".to_string(), 8),
            ("s8".to_string(), 8),
        ]
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn open_repairs_historical_tool_order_seq_duplicate_version() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir for tool-order repair")?;
    let subset_dir = tempdir.path().join("subset-migrations");
    fs::create_dir_all(&subset_dir).context("creating subset migration dir")?;

    let migrations = migration_files()?;
    for migration in &migrations {
        let version = migration_version(migration)?;
        if version <= 48 {
            let filename = migration
                .file_name()
                .context("migration file missing name")?;
            fs::copy(migration, subset_dir.join(filename)).with_context(|| {
                format!("copying {} into tool-order subset", migration.display())
            })?;
        }
    }

    let tool_order_migration = migrations
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "0055_tool_order_seq.sql")
        })
        .cloned()
        .context("finding tool order seq migration")?;
    fs::copy(
        &tool_order_migration,
        subset_dir.join("0049_tool_order_seq.sql"),
    )
    .with_context(|| {
        format!(
            "copying {} into tool-order subset as 0049",
            tool_order_migration.display()
        )
    })?;

    let db_path = tempdir.path().join("db.sqlite");
    fs::File::create(&db_path).context("creating sqlite file")?;
    let sqlite_url = sqlite_url(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .context("connecting partial-migration pool for tool-order repair")?;
    let subset_migrator = Migrator::new(subset_dir.clone())
        .await
        .context("loading subset migrator for tool-order repair")?;
    subset_migrator
        .run(&pool)
        .await
        .context("running subset migrator for tool-order repair")?;
    pool.close().await;

    let store = Store::open(&db_path)
        .await
        .context("opening store after historical tool-order migration")?;
    store.close().await;

    assert_store_integrity(&db_path).await?;
    assert!(column_exists(&db_path, "session_turn_tools", "order_seq").await?);

    let applied = applied_migrations(&db_path).await?;
    assert!(applied
        .iter()
        .any(|(version, description)| *version == 51
            && description == "drop workspace message index"));
    assert!(applied
        .iter()
        .any(|(version, description)| *version == 55 && description == "tool order seq"));

    Ok(())
}

#[tokio::test]
async fn open_repairs_partially_applied_session_subagent_archival_migration() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir for migration 64 repair")?;
    let subset_dir = tempdir.path().join("pre-64-migrations");
    fs::create_dir_all(&subset_dir).context("creating pre-64 migration dir")?;

    let migrations = migration_files()?;
    let migration_64 = migrations
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "0064_session_subagent_archival.sql")
        })
        .cloned()
        .context("finding session subagent archival migration")?;

    for migration in &migrations {
        if migration_version(migration)? <= 63 {
            let filename = migration
                .file_name()
                .context("migration file missing name")?;
            fs::copy(migration, subset_dir.join(filename))
                .with_context(|| format!("copying {} into pre-64 subset", migration.display()))?;
        }
    }

    let db_path = tempdir.path().join("db.sqlite");
    fs::File::create(&db_path).context("creating sqlite file")?;
    let sqlite_url = sqlite_url(&db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .context("connecting pre-64 pool")?;
    let subset_migrator = Migrator::new(subset_dir.clone())
        .await
        .context("loading pre-64 subset migrator")?;
    subset_migrator
        .run(&pool)
        .await
        .context("running pre-64 subset migrator")?;
    execute_migration_file_without_ledger(&pool, &migration_64)
        .await
        .context("applying migration 64 SQL without ledger row")?;

    let migration_64_recorded: i64 =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version = 64)")
            .fetch_one(&pool)
            .await
            .context("checking pre-repair migration 64 ledger")?;
    assert_eq!(
        migration_64_recorded, 0,
        "test setup should leave migration 64 schema applied but unrecorded"
    );
    pool.close().await;

    let store = Store::open(&db_path)
        .await
        .context("opening store after partial migration 64 application")?;
    store.close().await;

    assert_store_integrity(&db_path).await?;
    assert!(column_exists(&db_path, "sessions", "archived_at").await?);
    assert!(index_exists(&db_path, "idx_sessions_task_title_subagent_unique").await?);
    assert!(index_exists(&db_path, "idx_sessions_parent_relationship_active").await?);

    let applied = applied_migrations(&db_path).await?;
    assert!(applied.iter().any(|(version, description)| {
        *version == 64 && description == "session subagent archival"
    }));
    assert!(applied.iter().any(|(version, description)| {
        *version == 65 && description == "restore codex provider identity"
    }));

    Ok(())
}

#[tokio::test]
async fn sqlite_pragmas_and_integrity_hold_after_reopen() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let db_path = tempdir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.context("opening store")?;

    let workspace = store
        .create_workspace("hardening".into(), "/tmp/hardening".into(), VcsKind::Git)
        .await
        .context("creating workspace")?;
    let task = store
        .create_task(workspace.id, "exercise store".into(), None)
        .await
        .context("creating task")?;
    let worktree = store
        .create_worktree(
            workspace.id,
            "/tmp/hardening".into(),
            "deadbeef".into(),
            None,
        )
        .await
        .context("creating worktree")?;
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "model".into(),
            "assistant".into(),
            None,
            None,
            None,
        )
        .await
        .context("creating session")?;

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "hardening_check" }),
        )
        .await
        .context("appending session event")?;
    store
        .insert_message(Message {
            id: MessageId::new(),
            session_id: session.id,
            task_id: task.id,
            run_id: Some(run_id),
            turn_id: Some(turn_id),
            turn_sequence: None,
            order_seq: None,
            role: MessageRole::Assistant,
            content: "store hardening message".into(),
            attachments: vec![],
            delivery: MessageDelivery::Immediate,
            delivered_at: None,
            created_at: chrono::Utc::now(),
        })
        .await
        .context("inserting message")?;
    store.close().await;

    let reopened = Store::open(&db_path).await.context("reopening store")?;
    let events = reopened
        .list_session_events(session.id)
        .await
        .context("listing session events after reopen")?;
    let messages = reopened
        .list_messages_for_session(session.id)
        .await
        .context("listing messages after reopen")?;
    assert_eq!(events.len(), 1, "event log should survive reopen");
    assert_eq!(messages.len(), 1, "message write should survive reopen");
    let secure_delete_enabled: i64 = sqlx::query_scalar("PRAGMA secure_delete")
        .fetch_one(reopened.pool())
        .await
        .context("querying secure_delete on reopened store")?;
    assert_ne!(secure_delete_enabled, 0);
    reopened.close().await;

    assert_store_integrity(&db_path).await?;
    assert_eq!(journal_mode(&db_path).await?, "wal");

    Ok(())
}

#[tokio::test]
async fn cross_process_writes_wait_for_lock_release() -> Result<()> {
    if std::env::var_os(LOCK_CHILD_ENV).is_some() {
        return lock_holder_child().await;
    }

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let db_path = tempdir.path().join("db.sqlite");
    let ready_path = tempdir.path().join("lock.ready");
    let release_path = tempdir.path().join("lock.release");

    let bootstrap_store = Store::open(&db_path)
        .await
        .context("bootstrapping sqlite store before lock test")?;
    bootstrap_store.close().await;

    let exe = std::env::current_exe().context("locating current test binary")?;
    let mut child = Command::new(exe);
    child
        .arg("--exact")
        .arg("cross_process_writes_wait_for_lock_release")
        .arg("--nocapture")
        .env(LOCK_CHILD_ENV, "1")
        .env(LOCK_DB_PATH_ENV, &db_path)
        .env(LOCK_READY_PATH_ENV, &ready_path)
        .env(LOCK_RELEASE_PATH_ENV, &release_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = child
        .spawn()
        .context("spawning lock-holder child process")?;
    wait_for_file(&ready_path, Duration::from_secs(10))
        .await
        .context("waiting for lock-holder ready signal")?;

    let writer_store = Store::open(&db_path)
        .await
        .context("opening writer store while child holds lock")?;
    let started_at = Instant::now();
    let writer = tokio::spawn(async move {
        writer_store
            .create_workspace("contended".into(), "/tmp/contended".into(), VcsKind::Git)
            .await
    });

    sleep(Duration::from_millis(300)).await;
    assert!(
        !writer.is_finished(),
        "writer should still be waiting while the lock-holder keeps the transaction open"
    );

    fs::write(&release_path, b"release").context("writing release signal")?;
    let write_result = tokio::time::timeout(Duration::from_secs(10), writer)
        .await
        .context("timed out waiting for writer task")?
        .context("writer task join failed")?;
    write_result.context("writer should succeed after lock release")?;
    assert!(
        started_at.elapsed() >= Duration::from_millis(300),
        "writer should have observed lock contention before succeeding"
    );

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .context("timed out waiting for lock-holder child")?
        .context("waiting for lock-holder child failed")?;
    assert!(status.success(), "lock-holder child exited with {status}");

    assert_store_integrity(&db_path).await?;
    Ok(())
}

async fn lock_holder_child() -> Result<()> {
    let db_path = env_path(LOCK_DB_PATH_ENV)?;
    let ready_path = env_path(LOCK_READY_PATH_ENV)?;
    let release_path = env_path(LOCK_RELEASE_PATH_ENV)?;

    let store = Store::open(&db_path)
        .await
        .context("opening child store for lock holder")?;
    let mut conn = store
        .pool()
        .acquire()
        .await
        .context("acquiring child connection")?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *conn)
        .await
        .context("beginning immediate transaction in child")?;
    fs::write(&ready_path, b"ready").context("writing ready signal")?;

    wait_for_file(&release_path, Duration::from_secs(10))
        .await
        .context("waiting for release signal")?;

    sqlx::query("COMMIT")
        .execute(&mut *conn)
        .await
        .context("committing child transaction")?;
    store.close().await;
    Ok(())
}

async fn execute_sql_script(pool: &sqlx::SqlitePool, script: &str) -> Result<()> {
    for statement in script.split(';') {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        sqlx::query(statement)
            .execute(pool)
            .await
            .with_context(|| format!("executing SQL statement: {statement}"))?;
    }
    Ok(())
}

async fn insert_cleanup_event(
    pool: &sqlx::SqlitePool,
    id: &str,
    session_id: &str,
    original_type: &str,
    crp_channel: Option<&str>,
    transient: i64,
) -> Result<()> {
    let mut payload = serde_json::json!({
        "kind": "crp_unknown_event",
        "original_type": original_type,
    });
    if let Some(channel) = crp_channel {
        payload["crp_channel"] = serde_json::json!(channel);
    }
    sqlx::query(
        r#"INSERT INTO session_events (id, session_id, event_type, payload_json, transient)
           VALUES (?, ?, 'notice', ?, ?)"#,
    )
    .bind(id)
    .bind(session_id)
    .bind(payload.to_string())
    .bind(transient)
    .execute(pool)
    .await?;
    Ok(())
}

async fn assert_store_integrity(db_path: &Path) -> Result<()> {
    let sqlite_url = sqlite_url(db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .with_context(|| format!("connecting integrity pool for {}", db_path.display()))?;
    let quick_check: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&pool)
        .await
        .context("running PRAGMA quick_check")?;
    assert_eq!(quick_check.to_lowercase(), "ok");

    let foreign_key_rows = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .context("running PRAGMA foreign_key_check")?;
    assert!(
        foreign_key_rows.is_empty(),
        "expected foreign_key_check to be empty, found {} row(s)",
        foreign_key_rows.len()
    );
    pool.close().await;
    Ok(())
}

async fn journal_mode(db_path: &Path) -> Result<String> {
    let sqlite_url = sqlite_url(db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .with_context(|| format!("connecting journal_mode pool for {}", db_path.display()))?;
    let value: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .context("querying journal_mode")?;
    pool.close().await;
    Ok(value.to_lowercase())
}

async fn applied_migration_count(db_path: &Path) -> Result<i64> {
    let sqlite_url = sqlite_url(db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .with_context(|| format!("connecting migration count pool for {}", db_path.display()))?;
    let count: i64 =
        sqlx::query("SELECT COUNT(*) AS count FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(&pool)
            .await
            .context("counting applied migrations")?
            .get("count");
    pool.close().await;
    Ok(count)
}

async fn applied_migrations(db_path: &Path) -> Result<Vec<(i64, String)>> {
    let sqlite_url = sqlite_url(db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .with_context(|| {
            format!(
                "connecting migration details pool for {}",
                db_path.display()
            )
        })?;
    let rows = sqlx::query("SELECT version, description FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .context("listing applied migrations")?;
    pool.close().await;
    Ok(rows
        .into_iter()
        .map(|row| (row.get("version"), row.get("description")))
        .collect())
}

async fn column_exists(db_path: &Path, table: &str, column: &str) -> Result<bool> {
    let sqlite_url = sqlite_url(db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .with_context(|| format!("connecting column check pool for {}", db_path.display()))?;
    let query =
        format!("SELECT COUNT(*) AS count FROM pragma_table_info('{table}') WHERE name = ?");
    let count = sqlx::query_scalar::<_, i64>(&query)
        .bind(column)
        .fetch_one(&pool)
        .await
        .with_context(|| format!("checking {table}.{column}"))?;
    pool.close().await;
    Ok(count > 0)
}

async fn index_exists(db_path: &Path, index: &str) -> Result<bool> {
    let sqlite_url = sqlite_url(db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .with_context(|| format!("connecting index check pool for {}", db_path.display()))?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
    )
    .bind(index)
    .fetch_one(&pool)
    .await
    .with_context(|| format!("checking index {index}"))?;
    pool.close().await;
    Ok(count > 0)
}

async fn execute_migration_file_without_ledger(
    pool: &sqlx::SqlitePool,
    migration: &Path,
) -> Result<()> {
    let sql = fs::read_to_string(migration)
        .with_context(|| format!("reading migration {}", migration.display()))?;
    for statement in sql
        .split(";\n\n")
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement)
            .execute(pool)
            .await
            .with_context(|| {
                format!(
                    "executing statement from migration {}: {statement}",
                    migration.display()
                )
            })?;
    }
    Ok(())
}

async fn assert_index_columns(
    pool: &sqlx::SqlitePool,
    index_name: &str,
    expected_columns: &[&str],
) -> Result<()> {
    let query = format!("PRAGMA index_info('{index_name}')");
    let rows = sqlx::query(&query)
        .fetch_all(pool)
        .await
        .with_context(|| format!("reading index_info for {index_name}"))?;
    let actual_columns = rows
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<Vec<_>>();
    let expected_columns = expected_columns
        .iter()
        .map(|column| column.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        actual_columns, expected_columns,
        "unexpected column order for {index_name}"
    );
    Ok(())
}

async fn assert_query_plan_uses_index(
    pool: &sqlx::SqlitePool,
    label: &str,
    index_name: &str,
    sql: &str,
) -> Result<()> {
    let rows = sqlx::query(sql)
        .bind("session")
        .bind("turn")
        .fetch_all(pool)
        .await
        .with_context(|| format!("explaining {label}"))?;
    let details = rows
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        details.iter().any(|detail| detail.contains(index_name)),
        "expected {label} plan to use {index_name}; details: {details:?}"
    );
    Ok(())
}

fn migration_files() -> Result<Vec<PathBuf>> {
    let migrations_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files = fs::read_dir(&migrations_dir)
        .with_context(|| format!("reading migration dir {}", migrations_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("collecting migration paths")?;
    files.sort();
    Ok(files)
}

fn migration_version(path: &Path) -> Result<i64> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("migration path missing UTF-8 filename: {}", path.display()))?;
    let version = filename
        .split_once('_')
        .with_context(|| format!("migration filename missing version prefix: {filename}"))?
        .0
        .parse::<i64>()
        .with_context(|| format!("parsing migration version from {filename}"))?;
    Ok(version)
}

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.to_string_lossy())
}

fn env_path(var: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var(var).with_context(|| format!("missing env var {var}"))?,
    ))
}

async fn wait_for_file(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("timed out waiting for {}", path.display());
}

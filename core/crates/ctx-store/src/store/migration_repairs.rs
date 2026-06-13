use anyhow::{bail, Result};
use sqlx::{Pool, Sqlite};

const SESSION_SUBAGENT_ARCHIVAL_MIGRATION_VERSION: i64 = 64;
const SESSION_SUBAGENT_ARCHIVAL_MIGRATION_DESCRIPTION: &str = "session subagent archival";
const SESSION_SUBAGENT_ARCHIVAL_CHECKSUM_HEX: &str =
    "2857B7EE8D78E1549FD88D34B537E844FF22641650B7F6A79749F32553350BA05725D055DE4603171B03622E7CAA3AA9";
const TOOL_ORDER_SEQ_LEGACY_MIGRATION_VERSION: i64 = 49;
const TOOL_ORDER_SEQ_MIGRATION_VERSION: i64 = 55;
const TOOL_ORDER_SEQ_MIGRATION_DESCRIPTION: &str = "tool order seq";

async fn migrations_table_exists(pool: &Pool<Sqlite>) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations')",
    )
    .fetch_one(pool)
    .await?
        != 0)
}

async fn applied_migration_version(pool: &Pool<Sqlite>, description: &str) -> Result<Option<i64>> {
    sqlx::query_scalar::<_, i64>(
        "SELECT version FROM _sqlx_migrations WHERE description = ? LIMIT 1",
    )
    .bind(description)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn migration_description_for_version(
    pool: &Pool<Sqlite>,
    version: i64,
) -> Result<Option<String>> {
    sqlx::query_scalar::<_, String>(
        "SELECT description FROM _sqlx_migrations WHERE version = ? LIMIT 1",
    )
    .bind(version)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn column_exists(pool: &Pool<Sqlite>, table: &str, column: &str) -> Result<bool> {
    let sql = format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?");
    Ok(sqlx::query_scalar::<_, i64>(&sql)
        .bind(column)
        .fetch_one(pool)
        .await?
        > 0)
}

async fn index_exists(pool: &Pool<Sqlite>, index: &str) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?)",
    )
    .bind(index)
    .fetch_one(pool)
    .await?
        != 0)
}

pub(super) async fn repair_partial_session_subagent_archival_migration(
    pool: &Pool<Sqlite>,
) -> Result<()> {
    if !migrations_table_exists(pool).await? {
        return Ok(());
    }

    if migration_description_for_version(pool, SESSION_SUBAGENT_ARCHIVAL_MIGRATION_VERSION)
        .await?
        .is_some()
    {
        return Ok(());
    }

    if !column_exists(pool, "sessions", "archived_at").await? {
        return Ok(());
    }

    let unique_index_exists = index_exists(pool, "idx_sessions_task_title_subagent_unique").await?;
    let parent_index_exists = index_exists(pool, "idx_sessions_parent_relationship_active").await?;
    if !unique_index_exists || !parent_index_exists {
        bail!(
            "cannot repair partial migration {SESSION_SUBAGENT_ARCHIVAL_MIGRATION_VERSION}: sessions.archived_at exists but migration indexes are incomplete"
        );
    }

    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (?, ?, 1, ?, 0)",
    )
    .bind(SESSION_SUBAGENT_ARCHIVAL_MIGRATION_VERSION)
    .bind(SESSION_SUBAGENT_ARCHIVAL_MIGRATION_DESCRIPTION)
    .bind(decode_hex(SESSION_SUBAGENT_ARCHIVAL_CHECKSUM_HEX)?)
    .execute(pool)
    .await?;

    Ok(())
}

pub(super) async fn repair_historical_tool_order_seq_migration_version(
    pool: &Pool<Sqlite>,
) -> Result<()> {
    if !migrations_table_exists(pool).await? {
        return Ok(());
    }

    if applied_migration_version(pool, TOOL_ORDER_SEQ_MIGRATION_DESCRIPTION).await?
        != Some(TOOL_ORDER_SEQ_LEGACY_MIGRATION_VERSION)
    {
        return Ok(());
    }

    match migration_description_for_version(pool, TOOL_ORDER_SEQ_MIGRATION_VERSION).await? {
        None => {
            sqlx::query(
                "UPDATE _sqlx_migrations SET version = ? WHERE version = ? AND description = ?",
            )
            .bind(TOOL_ORDER_SEQ_MIGRATION_VERSION)
            .bind(TOOL_ORDER_SEQ_LEGACY_MIGRATION_VERSION)
            .bind(TOOL_ORDER_SEQ_MIGRATION_DESCRIPTION)
            .execute(pool)
            .await?;
        }
        Some(existing_description)
            if existing_description == TOOL_ORDER_SEQ_MIGRATION_DESCRIPTION =>
        {
            sqlx::query("DELETE FROM _sqlx_migrations WHERE version = ? AND description = ?")
                .bind(TOOL_ORDER_SEQ_LEGACY_MIGRATION_VERSION)
                .bind(TOOL_ORDER_SEQ_MIGRATION_DESCRIPTION)
                .execute(pool)
                .await?;
        }
        Some(existing_description) => {
            bail!(
                "cannot remap migration '{TOOL_ORDER_SEQ_MIGRATION_DESCRIPTION}' from version {TOOL_ORDER_SEQ_LEGACY_MIGRATION_VERSION} to {TOOL_ORDER_SEQ_MIGRATION_VERSION}: version {TOOL_ORDER_SEQ_MIGRATION_VERSION} is already occupied by '{existing_description}'"
            );
        }
    }

    Ok(())
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("hex literal has odd length");
    }

    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = decode_hex_nibble(pair[0])?;
            let low = decode_hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid hex digit"),
    }
}

use std::{env, path::PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_core::{database_path, SyncState, Visibility};
use ctx_history_store::Store;
use serde_json::json;
use tokio::runtime::Runtime;

use crate::{output::print_json, store_util::open_existing_store_read_only};

const DATABASE_URL_ENV: &str = "CTX_TURSO_DATABASE_URL";
const AUTH_TOKEN_ENV: &str = "CTX_TURSO_AUTH_TOKEN";
const DEFAULT_PUSH_BATCH_SIZE: usize = 100;
const MAX_PUSH_BATCH_SIZE: usize = 250;
const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_LIMIT: usize = 200;

#[derive(Debug, Args)]
pub(crate) struct TursoArgs {
    #[command(subcommand)]
    command: TursoCommand,
}

impl TursoArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            TursoCommand::Init(args) => args.json,
            TursoCommand::Push(args) => args.json,
            TursoCommand::Search(args) => args.json,
            TursoCommand::Status(args) => args.json,
        }
    }
}

#[derive(Debug, Subcommand)]
enum TursoCommand {
    #[command(about = "Create the portable remote ctx projection")]
    Init(TursoInitArgs),
    #[command(about = "Export an existing local ctx index to Turso")]
    Push(TursoPushArgs),
    #[command(about = "Search history stored in Turso")]
    Search(TursoSearchArgs),
    #[command(about = "Show Turso ctx projection status")]
    Status(TursoStatusArgs),
}

#[derive(Debug, Args)]
struct TursoInitArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct TursoPushArgs {
    #[arg(
        long,
        default_value_t = DEFAULT_PUSH_BATCH_SIZE,
        value_parser = parse_push_batch_size,
        help = "Number of events per remote transaction (1-250)"
    )]
    batch_size: usize,
    #[arg(long, help = "Upload no more than this many events")]
    limit: Option<usize>,
    #[arg(
        long,
        help = "Also export local-only events; required unless events are marked sync_full"
    )]
    include_local_only: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct TursoSearchArgs {
    #[arg(help = "ASCII case-insensitive substring query")]
    query: String,
    #[arg(
        long,
        help = "Filter by ctx provider name, for example codex or claude"
    )]
    provider: Option<String>,
    #[arg(long, default_value_t = DEFAULT_SEARCH_LIMIT, value_parser = parse_search_limit)]
    limit: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct TursoStatusArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug)]
struct TursoConfig {
    database_url: String,
    auth_token: String,
}

impl TursoConfig {
    fn from_env() -> Result<Self> {
        let database_url = required_env(DATABASE_URL_ENV)?;
        if !database_url.starts_with("libsql://") && !database_url.starts_with("https://") {
            return Err(anyhow!(
                "{DATABASE_URL_ENV} must start with libsql:// or https://"
            ));
        }
        Ok(Self {
            database_url,
            auth_token: required_env(AUTH_TOKEN_ENV)?,
        })
    }
}

pub(crate) fn run_turso(args: TursoArgs, data_root: PathBuf) -> Result<()> {
    match args.command {
        TursoCommand::Init(args) => run_async(init(args.json)),
        TursoCommand::Push(args) => {
            let db_path = database_path(data_root);
            let store = open_existing_store_read_only(&db_path, "ctx turso push")?;
            run_async(push(store, args))
        }
        TursoCommand::Search(args) => run_async(search(args)),
        TursoCommand::Status(args) => run_async(status(args.json)),
    }
}

fn run_async(operation: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    Runtime::new()
        .context("create async runtime for Turso")?
        .block_on(operation)
}

async fn connect() -> Result<libsql::Connection> {
    let config = TursoConfig::from_env()?;
    let database = libsql::Builder::new_remote(config.database_url, config.auth_token)
        .build()
        .await
        .context("connect to Turso")?;
    database.connect().context("open Turso connection")
}

async fn init(json_output: bool) -> Result<()> {
    let conn = connect().await?;
    ensure_schema(&conn).await?;
    if json_output {
        print_json(json!({"initialized": true, "remote_projection": true}))?;
    } else {
        println!("Turso ctx projection is ready.");
    }
    Ok(())
}

async fn ensure_schema(conn: &libsql::Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ctx_turso_events (\
            event_id TEXT PRIMARY KEY,\
            session_id TEXT,\
            provider TEXT NOT NULL,\
            role TEXT,\
            event_type TEXT NOT NULL,\
            occurred_at_ms INTEGER NOT NULL,\
            dedupe_key TEXT,\
            payload_json TEXT NOT NULL,\
            search_text TEXT NOT NULL DEFAULT ''\
        )",
        (),
    )
    .await
    .context("create Turso event table")?;
    ensure_dedupe_key_column(conn).await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS ctx_turso_events_occurred_at \
         ON ctx_turso_events(occurred_at_ms DESC)",
        (),
    )
    .await
    .context("create Turso event time index")?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS ctx_turso_events_provider \
         ON ctx_turso_events(provider)",
        (),
    )
    .await
    .context("create Turso provider index")?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS ctx_turso_events_dedupe_key \
         ON ctx_turso_events(dedupe_key) WHERE dedupe_key IS NOT NULL",
        (),
    )
    .await
    .context("create Turso dedupe index")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ctx_turso_upload_cursors (\
            device_id TEXT PRIMARY KEY,\
            last_event_id TEXT NOT NULL\
        )",
        (),
    )
    .await
    .context("create Turso upload cursor table")?;
    Ok(())
}

async fn ensure_dedupe_key_column(conn: &libsql::Connection) -> Result<()> {
    let mut columns = conn
        .query("PRAGMA table_info(ctx_turso_events)", ())
        .await
        .context("inspect Turso event table")?;
    while let Some(column) = columns.next().await.context("read Turso table column")? {
        if column.get::<String>(1)? == "dedupe_key" {
            return Ok(());
        }
    }
    conn.execute(
        "ALTER TABLE ctx_turso_events ADD COLUMN dedupe_key TEXT",
        (),
    )
    .await
    .context("add Turso event dedupe key")?;
    Ok(())
}

async fn push(store: Store, args: TursoPushArgs) -> Result<()> {
    let conn = connect().await?;
    ensure_schema(&conn).await?;
    let device_id = export_device_id(&store)?;
    let session_providers = store
        .list_sessions()?
        .into_iter()
        .map(|session| (session.id, session.provider.as_str().to_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let capture_source_providers = store
        .list_capture_sources()?
        .into_iter()
        .map(|source| (source.id, source.descriptor.provider.as_str().to_owned()))
        .collect::<std::collections::HashMap<_, _>>();

    let mut after_id = upload_cursor(&conn, &device_id).await?;
    let mut uploaded = 0u64;
    let mut skipped = 0usize;
    let mut scanned = 0usize;
    let mut batches = 0usize;
    loop {
        let remaining = args.limit.map(|limit| limit.saturating_sub(scanned));
        if remaining == Some(0) {
            break;
        }
        let page_size = remaining
            .map(|limit| limit.min(args.batch_size))
            .unwrap_or(args.batch_size);
        let events = store.list_events_page_after(after_id, page_size)?;
        let Some(last) = events.last() else {
            break;
        };
        scanned += events.len();
        after_id = Some(last.id);

        let transaction = conn
            .transaction()
            .await
            .context("begin Turso upload transaction")?;
        for event in &events {
            if !remote_export_allowed(event, args.include_local_only) {
                skipped += 1;
                continue;
            }
            let payload_json =
                serde_json::to_string(&event.payload).context("serialize event payload")?;
            let provider = event
                .session_id
                .and_then(|id| session_providers.get(&id))
                .or_else(|| {
                    event
                        .capture_source_id
                        .and_then(|id| capture_source_providers.get(&id))
                })
                .map(String::as_str)
                .unwrap_or("unknown");
            uploaded += transaction
                .execute(
                    "INSERT OR IGNORE INTO ctx_turso_events \
                     (event_id, session_id, provider, role, event_type, occurred_at_ms, dedupe_key, payload_json, search_text) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    libsql::params![
                        event.id.to_string(),
                        event.session_id.map(|id| id.to_string()),
                        provider,
                        event.role.map(|role| role.as_str().to_owned()),
                        event.event_type.as_str(),
                        event.occurred_at.timestamp_millis(),
                        event.dedupe_key.as_deref(),
                        payload_json,
                        "",
                    ],
                )
                .await
                .with_context(|| format!("upload event {}", event.id))?;
        }
        save_upload_cursor(&transaction, &device_id, last.id).await?;
        transaction
            .commit()
            .await
            .context("commit Turso upload transaction")?;
        batches += 1;
    }

    if args.json {
        print_json(json!({
            "uploaded_events": uploaded,
            "skipped_events": skipped,
            "scanned_events": scanned,
            "batches": batches,
            "idempotent": true,
            "remote_projection": true,
            "device_id": device_id,
        }))?;
    } else {
        println!("uploaded_events: {uploaded}");
        println!("skipped_events: {skipped}");
        println!("scanned_events: {scanned}");
        println!("batches: {batches}");
        println!("idempotent: true");
    }
    Ok(())
}

async fn search(args: TursoSearchArgs) -> Result<()> {
    let conn = connect().await?;
    require_projection(&conn).await?;
    let (sql, params) = if let Some(provider) = args.provider.as_deref() {
        (
            "SELECT event_id, session_id, provider, role, event_type, occurred_at_ms, payload_json \
             FROM ctx_turso_events WHERE provider = ?1 AND payload_json LIKE ?2 ESCAPE '\\' COLLATE NOCASE \
             ORDER BY occurred_at_ms DESC LIMIT ?3",
            libsql::params_from_iter(vec![
                libsql::Value::Text(provider.to_owned()),
                libsql::Value::Text(substring_pattern(&args.query)),
                libsql::Value::Integer(args.limit as i64),
            ]),
        )
    } else {
        (
            "SELECT event_id, session_id, provider, role, event_type, occurred_at_ms, payload_json \
             FROM ctx_turso_events WHERE payload_json LIKE ?1 ESCAPE '\\' COLLATE NOCASE \
             ORDER BY occurred_at_ms DESC LIMIT ?2",
            libsql::params_from_iter(vec![
                libsql::Value::Text(substring_pattern(&args.query)),
                libsql::Value::Integer(args.limit as i64),
            ]),
        )
    };
    let mut rows = conn
        .query(sql, params)
        .await
        .context("search Turso history")?;
    let mut results = Vec::new();
    while let Some(row) = rows.next().await.context("read Turso search result")? {
        results.push(json!({
            "event_id": row.get::<String>(0)?,
            "session_id": row.get::<Option<String>>(1)?,
            "provider": row.get::<String>(2)?,
            "role": row.get::<Option<String>>(3)?,
            "event_type": row.get::<String>(4)?,
            "occurred_at_ms": row.get::<i64>(5)?,
            "payload_json": row.get::<String>(6)?,
        }));
    }
    if args.json {
        print_json(json!({"query": args.query, "results": results}))?;
    } else {
        for result in results {
            println!("{}", serde_json::to_string(&result)?);
        }
    }
    Ok(())
}

async fn status(json_output: bool) -> Result<()> {
    let conn = connect().await?;
    require_projection(&conn).await?;
    let mut rows = conn
        .query(
            "SELECT COUNT(*), COUNT(DISTINCT provider), MIN(occurred_at_ms), MAX(occurred_at_ms) \
             FROM ctx_turso_events",
            (),
        )
        .await
        .context("read Turso status")?;
    let row = rows
        .next()
        .await
        .context("read Turso status row")?
        .ok_or_else(|| anyhow!("Turso status query returned no row"))?;
    let value = json!({
        "remote_projection": true,
        "events": row.get::<i64>(0)?,
        "providers": row.get::<i64>(1)?,
        "oldest_event_ms": row.get::<Option<i64>>(2)?,
        "newest_event_ms": row.get::<Option<i64>>(3)?,
    });
    if json_output {
        print_json(value)?;
    } else {
        for (key, value) in value.as_object().expect("status is an object") {
            println!("{key}: {value}");
        }
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| {
        format!("{name} is required; set it in your shell, never in a file or CLI argument")
    })
}

fn parse_push_batch_size(value: &str) -> std::result::Result<usize, String> {
    let size = value
        .parse::<usize>()
        .map_err(|error| format!("invalid batch size: {error}"))?;
    if !(1..=MAX_PUSH_BATCH_SIZE).contains(&size) {
        return Err(format!(
            "batch size must be between 1 and {MAX_PUSH_BATCH_SIZE}"
        ));
    }
    Ok(size)
}

fn parse_search_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|error| format!("invalid search limit: {error}"))?;
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(format!(
            "search limit must be between 1 and {MAX_SEARCH_LIMIT}"
        ));
    }
    Ok(limit)
}

fn substring_pattern(query: &str) -> String {
    format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

async fn require_projection(conn: &libsql::Connection) -> Result<()> {
    conn.query("SELECT 1 FROM ctx_turso_events LIMIT 1", ())
        .await
        .context(
            "open Turso ctx projection; run `ctx turso init` with a write-capable token first",
        )?;
    Ok(())
}

fn export_device_id(store: &Store) -> Result<String> {
    if let Ok(value) = env::var("CTX_TURSO_DEVICE_ID") {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    store
        .local_device()?
        .map(|device| device.stable_device_id)
        .ok_or_else(|| {
            anyhow!(
                "CTX_TURSO_DEVICE_ID is required when the local ctx store has no device identity"
            )
        })
}

async fn upload_cursor(conn: &libsql::Connection, device_id: &str) -> Result<Option<uuid::Uuid>> {
    let mut rows = conn
        .query(
            "SELECT last_event_id FROM ctx_turso_upload_cursors WHERE device_id = ?1",
            [device_id],
        )
        .await
        .context("read Turso upload cursor")?;
    let Some(row) = rows.next().await.context("read Turso upload cursor row")? else {
        return Ok(None);
    };
    row.get::<String>(0)?
        .parse()
        .map(Some)
        .context("parse Turso upload cursor")
}

async fn save_upload_cursor(
    conn: &libsql::Connection,
    device_id: &str,
    last_event_id: uuid::Uuid,
) -> Result<()> {
    conn.execute(
        "INSERT INTO ctx_turso_upload_cursors (device_id, last_event_id) VALUES (?1, ?2) \
         ON CONFLICT(device_id) DO UPDATE SET last_event_id = excluded.last_event_id",
        (device_id, last_event_id.to_string()),
    )
    .await
    .context("save Turso upload cursor")?;
    Ok(())
}

fn remote_export_allowed(event: &ctx_history_core::Event, include_local_only: bool) -> bool {
    if matches!(event.sync.visibility, Visibility::Withheld)
        || matches!(event.sync.sync_state, SyncState::Withheld)
    {
        return false;
    }
    matches!(event.sync.visibility, Visibility::SyncFull) || include_local_only
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bounded_push_batch_size() {
        assert_eq!(parse_push_batch_size("1"), Ok(1));
        assert_eq!(parse_push_batch_size("250"), Ok(250));
        assert!(parse_push_batch_size("0").is_err());
        assert!(parse_push_batch_size("251").is_err());
    }

    #[test]
    fn validates_bounded_search_limit() {
        assert_eq!(parse_search_limit("200"), Ok(200));
        assert!(parse_search_limit("0").is_err());
        assert!(parse_search_limit("201").is_err());
    }

    #[test]
    fn escapes_like_wildcards_in_queries() {
        assert_eq!(substring_pattern("50%_off\\now"), "%50\\%\\_off\\\\now%");
    }
}

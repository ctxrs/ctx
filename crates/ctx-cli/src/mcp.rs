#![allow(unused_imports)]
use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};

use clap::{Args, Subcommand};

use ctx_history_core::{database_path, EventType};

use ctx_history_store::{
    RawSqlOptions, Store, RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS,
    RAW_SQL_DEFAULT_MAX_SQL_BYTES, RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT,
    RAW_SQL_MAX_COLUMNS_CAP, RAW_SQL_MAX_ROWS_CAP, RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT,
    RAW_SQL_MAX_VALUE_BYTES_CAP,
};

use serde_json::{json, Value};

use uuid::Uuid;

use super::{
    cli_supported_provider, compact_json, config::CONFIG_FILE, discovered_plugin_sources_json,
    discovered_sources, event_window, event_window_json, indexed_history_item_count,
    mark_share_safe, raw_sql_result_json, search_filters, search_has_intent,
    session_transcript_json, sources_json, OutputFormat, ProviderArg, RefreshArg, SearchDto,
    SearchFilterInput, SearchIntentInput, SearchRefreshReport, SourceIdentityFilterArgs,
    TranscriptMode, MAX_EVENT_WINDOW, MAX_SEARCH_LIMIT,
};

#[path = "mcp/mcp.rs"]
mod cli_mcp_mcp;
pub(crate) use cli_mcp_mcp::*;

#[path = "mcp/session.rs"]
mod cli_mcp_session;
pub(crate) use cli_mcp_session::*;

#[path = "mcp/discard.rs"]
mod cli_mcp_discard;
pub(crate) use cli_mcp_discard::*;

#[path = "mcp/path.rs"]
mod cli_mcp_path;
pub(crate) use cli_mcp_path::*;

#[path = "mcp/catalog.rs"]
mod cli_mcp_catalog;
pub(crate) use cli_mcp_catalog::*;

#[path = "mcp/schema.rs"]
mod cli_mcp_schema;
pub(crate) use cli_mcp_schema::*;

#[path = "mcp/search.rs"]
mod cli_mcp_search;
pub(crate) use cli_mcp_search::*;

#[path = "mcp/raw_sql.rs"]
mod cli_mcp_raw_sql;
pub(crate) use cli_mcp_raw_sql::*;

#[path = "mcp/event.rs"]
mod cli_mcp_event;
pub(crate) use cli_mcp_event::*;

#[path = "mcp/import.rs"]
mod cli_mcp_import;
pub(crate) use cli_mcp_import::*;

#[path = "mcp/json.rs"]
mod cli_mcp_json;
pub(crate) use cli_mcp_json::*;

#[path = "mcp/optional.rs"]
mod cli_mcp_optional;
pub(crate) use cli_mcp_optional::*;

#[path = "mcp/duration.rs"]
mod cli_mcp_duration;
pub(crate) use cli_mcp_duration::*;

#[path = "mcp/required.rs"]
mod cli_mcp_required;
pub(crate) use cli_mcp_required::*;

#[path = "mcp/provider.rs"]
mod cli_mcp_provider;
pub(crate) use cli_mcp_provider::*;

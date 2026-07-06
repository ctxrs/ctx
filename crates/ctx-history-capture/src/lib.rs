#![allow(unused_imports)]
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Duration, NaiveDateTime, Utc};

use ctx_history_core::{
    inbox_dir as core_inbox_dir, new_id, utc_now, AgentType, CaptureEnvelope, CaptureProvider,
    CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence,
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord,
    EntityTimestamps, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    HistoryRecord, ProviderCaptureEnvelope, ProviderCursorCheckpoint, ProviderCursorRange,
    ProviderEventEnvelope, ProviderRawRetention, ProviderRedactionBoundary,
    ProviderSessionEnvelope, ProviderSourceEnvelope, ProviderSourceTrust, RedactionState, Run,
    RunStatus, RunType, Session, SessionEdge, SessionEdgeType, SessionHistoryArchive,
    SessionStatus, SyncCursor, SyncMetadata, SyncState, Visibility,
    CTX_HISTORY_JSONL_V1_SCHEMA_VERSION, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};

use ctx_history_store::{CatalogSession, Store, StoreError};

use rmpv::{decode::read_value as read_msgpack_value, Value as MsgpackValue};

use rusqlite::{limits::Limit, Connection, OpenFlags, OptionalExtension};

use serde::{Deserialize, Serialize};

use serde_json::{json, Value};

use thiserror::Error;

use uuid::Uuid;

pub mod provider_sources;

pub use provider_sources::{
    discover_provider_sources, discover_provider_sources_for_provider, provider_source_for_path,
    provider_source_spec, provider_source_specs, ProviderCatalogSupport, ProviderDefaultLocation,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceSpec,
    ProviderSourceStatus,
};

#[path = "capture/schema.rs"]
mod capture_schema;
pub use capture_schema::CAPTURE_SCHEMA_VERSION;
pub(crate) use capture_schema::*;

#[path = "capture/provider.rs"]
mod capture_provider;
pub(crate) use capture_provider::*;

#[path = "capture/sqlite.rs"]
mod capture_sqlite;
pub(crate) use capture_sqlite::*;
pub use capture_sqlite::{
    AstrBotSqliteAdapter, AstrBotSqliteImportOptions, CaptureError, DeepAgentsSqliteAdapter,
    DeepAgentsSqliteImportOptions, ForgeCodeSqliteAdapter, ForgeCodeSqliteImportOptions,
    OpenCodeSqliteAdapter, OpenCodeSqliteImportOptions,
};

#[path = "capture/openclaw.rs"]
mod capture_openclaw;
pub use capture_openclaw::import_openclaw_history;
pub(crate) use capture_openclaw::*;

#[path = "capture/error.rs"]
mod capture_error;
pub use capture_error::Result;
pub(crate) use capture_error::*;

#[path = "capture/spool.rs"]
mod capture_spool;
pub(crate) use capture_spool::*;
pub use capture_spool::{
    import_spool, read_jsonl, retry_failed_spool_files, spool_counts, SpoolCounts,
    SpoolImportFailure, SpoolImportSummary, SpoolRepairSummary, SpoolWriter,
};

#[path = "capture/fixture.rs"]
mod capture_fixture;
pub(crate) use capture_fixture::*;
pub use capture_fixture::{
    fixture_envelope, import_provider_fixture_jsonl, write_fixture, FixtureOptions,
    ProviderFixtureImportOptions, ProviderFixtureJsonlAdapter, ProviderFixtureLine,
};

#[path = "capture/custom_history_01.rs"]
mod capture_custom_history_01;
pub(crate) use capture_custom_history_01::*;
pub use capture_custom_history_01::{
    import_custom_history_jsonl_v1, import_custom_history_jsonl_v1_reader,
    validate_custom_history_jsonl_v1, validate_custom_history_jsonl_v1_reader,
    CustomHistoryJsonlV1ImportOptions,
};

#[path = "capture/custom_history_02.rs"]
mod capture_custom_history_02;
pub(crate) use capture_custom_history_02::*;

#[path = "capture/import_01.rs"]
mod capture_import_01;
pub(crate) use capture_import_01::*;
pub use capture_import_01::{
    import_continue_cli_sessions, import_normalized_provider_captures, ProviderImportFailure,
    ProviderImportSummary, ProviderNormalizationResult,
};

#[path = "capture/import_02.rs"]
mod capture_import_02;
pub(crate) use capture_import_02::*;

#[path = "capture/codex_01.rs"]
mod capture_codex_01;
pub(crate) use capture_codex_01::*;
pub use capture_codex_01::{
    CodexEventImportMode, CodexHistoryImportOptions, CodexHistoryJsonlAdapter,
    CodexSessionCatalogOptions, CodexSessionImportOptions, CodexSessionImportProgress,
    CodexSessionImportProgressCallback, CodexSessionJsonlAdapter, CodexToolOutputMode,
    ProviderAdapterContext,
};

#[path = "capture/codex_02.rs"]
mod capture_codex_02;
pub(crate) use capture_codex_02::*;
pub use capture_codex_02::{
    import_codex_history_jsonl, import_codex_session_jsonl, import_codex_session_jsonl_tail,
    import_codex_session_paths, import_codex_session_tree,
};

#[path = "capture/codex_03.rs"]
mod capture_codex_03;
pub use capture_codex_03::catalog_codex_session_tree;
pub(crate) use capture_codex_03::*;

#[path = "capture/codex_04.rs"]
mod capture_codex_04;
pub(crate) use capture_codex_04::*;

#[path = "capture/codex_05.rs"]
mod capture_codex_05;
pub(crate) use capture_codex_05::*;

#[path = "capture/catalog.rs"]
mod capture_catalog;
pub use capture_catalog::CatalogSummary;
pub(crate) use capture_catalog::*;

#[path = "capture/pi.rs"]
mod capture_pi;
pub(crate) use capture_pi::*;
pub use capture_pi::{import_pi_session_jsonl, PiSessionImportOptions, PiSessionJsonlAdapter};

#[path = "capture/claude.rs"]
mod capture_claude;
pub(crate) use capture_claude::*;
pub use capture_claude::{
    import_claude_projects_jsonl_tree, ClaudeProjectsImportOptions, ClaudeProjectsJsonlAdapter,
};

#[path = "capture/cline.rs"]
mod capture_cline;
pub(crate) use capture_cline::*;
pub use capture_cline::{
    import_cline_task_json_history, ClineTaskJsonAdapter, ClineTaskJsonImportOptions,
};

#[path = "capture/roo.rs"]
mod capture_roo;
pub(crate) use capture_roo::*;
pub use capture_roo::{import_roo_task_json_history, RooTaskJsonAdapter, RooTaskJsonImportOptions};

#[path = "capture/code_buddy_01.rs"]
mod capture_code_buddy_01;
pub(crate) use capture_code_buddy_01::*;
pub use capture_code_buddy_01::{
    import_codebuddy_history, CodeBuddyHistoryJsonAdapter, CodeBuddyImportOptions,
};

#[path = "capture/code_buddy_02.rs"]
mod capture_code_buddy_02;
pub(crate) use capture_code_buddy_02::*;

#[path = "capture/auggie.rs"]
mod capture_auggie;
pub(crate) use capture_auggie::*;
pub use capture_auggie::{import_auggie_history, AuggieImportOptions, AuggieSessionJsonAdapter};

#[path = "capture/junie_01.rs"]
mod capture_junie_01;
pub(crate) use capture_junie_01::*;
pub use capture_junie_01::{import_junie_history, JunieImportOptions, JunieSessionEventsAdapter};

#[path = "capture/junie_02.rs"]
mod capture_junie_02;
pub(crate) use capture_junie_02::*;

#[path = "capture/firebender.rs"]
mod capture_firebender;
pub(crate) use capture_firebender::*;
pub use capture_firebender::{
    import_firebender_sqlite, FirebenderSqliteAdapter, FirebenderSqliteImportOptions,
};

#[path = "capture/kilo.rs"]
mod capture_kilo;
pub(crate) use capture_kilo::*;
pub use capture_kilo::{import_kilo_sqlite, KiloSqliteAdapter, KiloSqliteImportOptions};

#[path = "capture/kiro.rs"]
mod capture_kiro;
pub(crate) use capture_kiro::*;
pub use capture_kiro::{import_kiro_sqlite, KiroSqliteAdapter, KiroSqliteImportOptions};

#[path = "capture/crush_01.rs"]
mod capture_crush_01;
pub(crate) use capture_crush_01::*;
pub use capture_crush_01::{import_crush_sqlite, CrushSqliteAdapter, CrushSqliteImportOptions};

#[path = "capture/crush_02.rs"]
mod capture_crush_02;
pub(crate) use capture_crush_02::*;

#[path = "capture/goose_01.rs"]
mod capture_goose_01;
pub(crate) use capture_goose_01::*;
pub use capture_goose_01::{
    import_goose_sessions_sqlite, GooseSessionsSqliteAdapter, GooseSessionsSqliteImportOptions,
};

#[path = "capture/goose_02.rs"]
mod capture_goose_02;
pub(crate) use capture_goose_02::*;

#[path = "capture/record.rs"]
mod capture_record;
pub(crate) use capture_record::*;
pub use capture_record::{
    ContinueCliImportOptions, NanoClawImportOptions, OpenClawImportOptions, OpenHandsImportOptions,
};

#[path = "capture/hermes.rs"]
mod capture_hermes;
pub(crate) use capture_hermes::*;
pub use capture_hermes::{import_hermes_sqlite, HermesSqliteAdapter, HermesSqliteImportOptions};

#[path = "capture/shelley_01.rs"]
mod capture_shelley_01;
pub(crate) use capture_shelley_01::*;
pub use capture_shelley_01::{
    import_shelley_sqlite, ShelleySqliteAdapter, ShelleySqliteImportOptions,
};

#[path = "capture/shelley_02.rs"]
mod capture_shelley_02;
pub(crate) use capture_shelley_02::*;

#[path = "capture/warp_01.rs"]
mod capture_warp_01;
pub(crate) use capture_warp_01::*;
pub use capture_warp_01::{import_warp_sqlite, WarpSqliteImportOptions};

#[path = "capture/warp_02.rs"]
mod capture_warp_02;
pub(crate) use capture_warp_02::*;

#[path = "capture/lingma.rs"]
mod capture_lingma;
pub(crate) use capture_lingma::*;
pub use capture_lingma::{import_lingma_sqlite, LingmaSqliteAdapter, LingmaSqliteImportOptions};

#[path = "capture/trae_01.rs"]
mod capture_trae_01;
pub(crate) use capture_trae_01::*;
pub use capture_trae_01::{import_trae_history, TraeImportOptions};

#[path = "capture/trae_02.rs"]
mod capture_trae_02;
pub(crate) use capture_trae_02::*;

#[path = "capture/antigravity.rs"]
mod capture_antigravity;
pub(crate) use capture_antigravity::*;
pub use capture_antigravity::{
    import_antigravity_cli_history, AntigravityCliImportOptions, AntigravityCliJsonlAdapter,
};

#[path = "capture/gemini.rs"]
mod capture_gemini;
pub(crate) use capture_gemini::*;
pub use capture_gemini::{
    import_gemini_cli_history, GeminiCliImportOptions, GeminiCliJsonlAdapter,
    TabnineCliImportOptions,
};

#[path = "capture/factory.rs"]
mod capture_factory;
pub(crate) use capture_factory::*;
pub use capture_factory::{
    import_factory_ai_droid_sessions, FactoryAiDroidImportOptions, FactoryAiDroidJsonlAdapter,
};

#[path = "capture/copilot.rs"]
mod capture_copilot;
pub(crate) use capture_copilot::*;
pub use capture_copilot::{
    import_copilot_cli_session_events, CopilotCliImportOptions, CopilotCliSessionEventsAdapter,
};

#[path = "capture/cursor.rs"]
mod capture_cursor;
pub(crate) use capture_cursor::*;
pub use capture_cursor::{
    custom_history_jsonl_v1_cursor_stream, import_cursor_native_history,
    CursorAgentTranscriptJsonlAdapter, CursorNativeImportOptions,
};

#[path = "capture/windsurf.rs"]
mod capture_windsurf;
pub(crate) use capture_windsurf::*;
pub use capture_windsurf::{
    import_windsurf_cascade_hook_transcripts, WindsurfCascadeHookImportOptions,
    WindsurfCascadeHookTranscriptJsonlAdapter,
};

#[path = "capture/qoder.rs"]
mod capture_qoder;
pub(crate) use capture_qoder::*;
pub use capture_qoder::{import_qoder_history, QoderImportOptions, QoderJsonlAdapter};

#[path = "capture/zed_01.rs"]
mod capture_zed_01;
pub(crate) use capture_zed_01::*;
pub use capture_zed_01::{
    import_zed_threads_sqlite, ZedThreadsSqliteAdapter, ZedThreadsSqliteImportOptions,
};

#[path = "capture/zed_02.rs"]
mod capture_zed_02;
pub(crate) use capture_zed_02::*;

#[path = "capture/qwen.rs"]
mod capture_qwen;
pub(crate) use capture_qwen::*;
pub use capture_qwen::{import_qwen_code_history, QwenCodeImportOptions, QwenCodeJsonlAdapter};

#[path = "capture/kimi_01.rs"]
mod capture_kimi_01;
pub(crate) use capture_kimi_01::*;
pub use capture_kimi_01::{
    import_kimi_code_cli_history, KimiCodeCliImportOptions, KimiCodeCliWireJsonlAdapter,
};

#[path = "capture/kimi_02.rs"]
mod capture_kimi_02;
pub(crate) use capture_kimi_02::*;

#[path = "capture/rovodev.rs"]
mod capture_rovodev;
pub(crate) use capture_rovodev::*;
pub use capture_rovodev::{
    import_rovodev_history, RovoDevImportOptions, RovoDevSessionJsonAdapter,
};

#[path = "capture/mistral.rs"]
mod capture_mistral;
pub(crate) use capture_mistral::*;
pub use capture_mistral::{
    import_mistral_vibe_history, MistralVibeImportOptions, MistralVibeJsonlAdapter,
};

#[path = "capture/mux_01.rs"]
mod capture_mux_01;
pub(crate) use capture_mux_01::*;
pub use capture_mux_01::{import_mux_history, MuxImportOptions, MuxJsonlAdapter};

#[path = "capture/mux_02.rs"]
mod capture_mux_02;
pub(crate) use capture_mux_02::*;

#[path = "capture/session_01.rs"]
mod capture_session_01;
pub(crate) use capture_session_01::*;
pub use capture_session_01::{ProviderFileTouchedEnvelope, ProviderSessionDto};

#[path = "capture/session_02.rs"]
mod capture_session_02;
pub(crate) use capture_session_02::*;

#[path = "capture/event.rs"]
mod capture_event;
pub(crate) use capture_event::*;
pub use capture_event::{NormalizedProviderImportOptions, ProviderEventDto};

#[path = "capture/path_01.rs"]
mod capture_path_01;
pub(crate) use capture_path_01::*;
pub use capture_path_01::{inbox_dir, ProviderCaptureAdapter};

#[path = "capture/path_02.rs"]
mod capture_path_02;
pub(crate) use capture_path_02::*;

#[path = "capture/open.rs"]
mod capture_open;
pub(crate) use capture_open::*;
pub use capture_open::{OpenClawJsonlAdapter, OpenHandsFileEventsAdapter};

#[path = "capture/nano.rs"]
mod capture_nano;
pub use capture_nano::NanoClawProjectAdapter;
pub(crate) use capture_nano::*;

#[path = "capture/continue_mod.rs"]
mod capture_continue_mod;
pub use capture_continue_mod::ContinueCliSessionsAdapter;
pub(crate) use capture_continue_mod::*;

#[path = "capture/tabnine.rs"]
mod capture_tabnine;
pub(crate) use capture_tabnine::*;
pub use capture_tabnine::{import_tabnine_cli_history, TabnineCliJsonlAdapter};

#[path = "capture/contains.rs"]
mod capture_contains;
pub(crate) use capture_contains::*;

#[path = "capture/find.rs"]
mod capture_find;
pub(crate) use capture_find::*;

#[path = "capture/opencode_01.rs"]
mod capture_opencode_01;
pub use capture_opencode_01::import_opencode_sqlite;
pub(crate) use capture_opencode_01::*;

#[path = "capture/opencode_02.rs"]
mod capture_opencode_02;
pub(crate) use capture_opencode_02::*;

#[path = "capture/nanoclaw.rs"]
mod capture_nanoclaw;
pub use capture_nanoclaw::import_nanoclaw_project;
pub(crate) use capture_nanoclaw::*;

#[path = "capture/astrbot.rs"]
mod capture_astrbot;
pub use capture_astrbot::import_astrbot_sqlite;
pub(crate) use capture_astrbot::*;

#[path = "capture/openhands.rs"]
mod capture_openhands;
pub use capture_openhands::import_openhands_file_events;
pub(crate) use capture_openhands::*;

#[path = "capture/forgecode_01.rs"]
mod capture_forgecode_01;
pub use capture_forgecode_01::import_forgecode_sqlite;
pub(crate) use capture_forgecode_01::*;

#[path = "capture/forgecode_02.rs"]
mod capture_forgecode_02;
pub(crate) use capture_forgecode_02::*;

#[path = "capture/deepagents.rs"]
mod capture_deepagents;
pub use capture_deepagents::import_deepagents_sqlite;
pub(crate) use capture_deepagents::*;

#[path = "capture/archive.rs"]
mod capture_archive;
pub use capture_archive::archive_from_envelopes;
pub(crate) use capture_archive::*;

#[path = "capture/time.rs"]
mod capture_time;
pub(crate) use capture_time::*;

#[path = "capture/native_jsonl.rs"]
mod capture_native_jsonl;
pub(crate) use capture_native_jsonl::*;

#[path = "capture/json.rs"]
mod capture_json;
pub use capture_json::compute_payload_hash;
pub(crate) use capture_json::*;

#[path = "capture/capped.rs"]
mod capture_capped;
pub(crate) use capture_capped::*;

#[path = "capture/collect.rs"]
mod capture_collect;
pub(crate) use capture_collect::*;

#[path = "capture/value.rs"]
mod capture_value;
pub(crate) use capture_value::*;

#[path = "capture/normalized.rs"]
mod capture_normalized;
pub(crate) use capture_normalized::*;

#[path = "capture/provider_source.rs"]
mod capture_provider_source;
pub(crate) use capture_provider_source::*;

#[path = "capture/remove.rs"]
mod capture_remove;
pub(crate) use capture_remove::*;

#[path = "capture/text.rs"]
mod capture_text;
pub(crate) use capture_text::*;

#[path = "capture/deep.rs"]
mod capture_deep;
pub(crate) use capture_deep::*;

#[path = "capture/msgpack.rs"]
mod capture_msgpack;
pub(crate) use capture_msgpack::*;

#[path = "capture/status.rs"]
mod capture_status;
pub(crate) use capture_status::*;

#[path = "capture/proto.rs"]
mod capture_proto;
pub(crate) use capture_proto::*;

#[path = "capture/astr.rs"]
mod capture_astr;
pub(crate) use capture_astr::*;

#[path = "capture/optional.rs"]
mod capture_optional;
pub(crate) use capture_optional::*;

#[path = "capture/forge.rs"]
mod capture_forge;
pub(crate) use capture_forge::*;

#[path = "capture/droid.rs"]
mod capture_droid;
pub(crate) use capture_droid::*;

#[path = "capture/stable.rs"]
mod capture_stable;
pub use capture_stable::stable_capture_uuid;
pub(crate) use capture_stable::*;

#[path = "capture/string.rs"]
mod capture_string;
pub(crate) use capture_string::*;

#[path = "capture/sql.rs"]
mod capture_sql;
pub(crate) use capture_sql::*;

#[path = "capture/source_identity.rs"]
mod capture_source_identity;
pub(crate) use capture_source_identity::*;

#[path = "capture/sanitize.rs"]
mod capture_sanitize;
pub(crate) use capture_sanitize::*;

#[path = "capture/default.rs"]
mod capture_default;
pub(crate) use capture_default::*;

#[path = "capture/fnv1a64.rs"]
mod capture_fnv1a64;
pub(crate) use capture_fnv1a64::*;

#[cfg(test)]
#[path = "capture_tests/tests/mod.rs"]
mod tests;

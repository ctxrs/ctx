#![allow(unused_imports)]
use super::*;

use tempfile::TempDir;

#[path = "tempdir.rs"]
mod capture_tests_tempdir;
pub(crate) use capture_tests_tempdir::*;

#[path = "fixture.rs"]
mod capture_tests_fixture;
pub(crate) use capture_tests_fixture::*;

#[path = "custom_history.rs"]
mod capture_tests_custom_history;
pub(crate) use capture_tests_custom_history::*;

#[path = "path.rs"]
mod capture_tests_path;
pub(crate) use capture_tests_path::*;

#[path = "json.rs"]
mod capture_tests_json;
pub(crate) use capture_tests_json::*;

#[path = "cursor.rs"]
mod capture_tests_cursor;
pub(crate) use capture_tests_cursor::*;

#[path = "codex_01.rs"]
mod capture_tests_codex_01;
pub(crate) use capture_tests_codex_01::*;

#[path = "codex_02.rs"]
mod capture_tests_codex_02;
pub(crate) use capture_tests_codex_02::*;

#[path = "codex_03.rs"]
mod capture_tests_codex_03;
pub(crate) use capture_tests_codex_03::*;

#[path = "catalog.rs"]
mod capture_tests_catalog;
pub(crate) use capture_tests_catalog::*;

#[path = "timing.rs"]
mod capture_tests_timing;
pub(crate) use capture_tests_timing::*;

#[path = "percentile.rs"]
mod capture_tests_percentile;
pub(crate) use capture_tests_percentile::*;

#[path = "time.rs"]
mod capture_tests_time;
pub(crate) use capture_tests_time::*;

#[path = "rounded.rs"]
mod capture_tests_rounded;
pub(crate) use capture_tests_rounded::*;

#[path = "env.rs"]
mod capture_tests_env;
pub(crate) use capture_tests_env::*;

#[path = "import.rs"]
mod capture_tests_import;
pub(crate) use capture_tests_import::*;

#[path = "sqlite.rs"]
mod capture_tests_sqlite;
pub(crate) use capture_tests_sqlite::*;

#[path = "spool.rs"]
mod capture_tests_spool;
pub(crate) use capture_tests_spool::*;

#[path = "pi.rs"]
mod capture_tests_pi;
pub(crate) use capture_tests_pi::*;

#[path = "claude.rs"]
mod capture_tests_claude;
pub(crate) use capture_tests_claude::*;

#[path = "opencode.rs"]
mod capture_tests_opencode;
pub(crate) use capture_tests_opencode::*;

#[path = "antigravity.rs"]
mod capture_tests_antigravity;
pub(crate) use capture_tests_antigravity::*;

#[path = "windsurf.rs"]
mod capture_tests_windsurf;
pub(crate) use capture_tests_windsurf::*;

#[path = "qoder.rs"]
mod capture_tests_qoder;
pub(crate) use capture_tests_qoder::*;

#[path = "cline.rs"]
mod capture_tests_cline;
pub(crate) use capture_tests_cline::*;

#[path = "code_buddy.rs"]
mod capture_tests_code_buddy;
pub(crate) use capture_tests_code_buddy::*;

#[path = "trae.rs"]
mod capture_tests_trae;
pub(crate) use capture_tests_trae::*;

#[path = "auggie.rs"]
mod capture_tests_auggie;
pub(crate) use capture_tests_auggie::*;

#[path = "firebender.rs"]
mod capture_tests_firebender;
pub(crate) use capture_tests_firebender::*;

#[path = "lingma.rs"]
mod capture_tests_lingma;
pub(crate) use capture_tests_lingma::*;

#[path = "kilo.rs"]
mod capture_tests_kilo;
pub(crate) use capture_tests_kilo::*;

#[path = "warp.rs"]
mod capture_tests_warp;
pub(crate) use capture_tests_warp::*;

#[path = "hermes.rs"]
mod capture_tests_hermes;
pub(crate) use capture_tests_hermes::*;

#[path = "openclaw.rs"]
mod capture_tests_openclaw;
pub(crate) use capture_tests_openclaw::*;

#[path = "shelley.rs"]
mod capture_tests_shelley;
pub(crate) use capture_tests_shelley::*;

#[path = "crush.rs"]
mod capture_tests_crush;
pub(crate) use capture_tests_crush::*;

#[path = "goose.rs"]
mod capture_tests_goose;
pub(crate) use capture_tests_goose::*;

#[path = "kiro.rs"]
mod capture_tests_kiro;
pub(crate) use capture_tests_kiro::*;

#[path = "astrbot.rs"]
mod capture_tests_astrbot;
pub(crate) use capture_tests_astrbot::*;

#[path = "junie.rs"]
mod capture_tests_junie;
pub(crate) use capture_tests_junie::*;

#[path = "zed.rs"]
mod capture_tests_zed;
pub(crate) use capture_tests_zed::*;

#[path = "forgecode.rs"]
mod capture_tests_forgecode;
pub(crate) use capture_tests_forgecode::*;

#[path = "deepagents.rs"]
mod capture_tests_deepagents;
pub(crate) use capture_tests_deepagents::*;

#[path = "mistral.rs"]
mod capture_tests_mistral;
pub(crate) use capture_tests_mistral::*;

#[path = "mux.rs"]
mod capture_tests_mux;
pub(crate) use capture_tests_mux::*;

#[path = "rovodev.rs"]
mod capture_tests_rovodev;
pub(crate) use capture_tests_rovodev::*;

#[path = "copilot.rs"]
mod capture_tests_copilot;
pub(crate) use capture_tests_copilot::*;

#[path = "kimi.rs"]
mod capture_tests_kimi;
pub(crate) use capture_tests_kimi::*;

#[path = "gemini.rs"]
mod capture_tests_gemini;
pub(crate) use capture_tests_gemini::*;

#[path = "native_jsonl.rs"]
mod capture_tests_native_jsonl;
pub(crate) use capture_tests_native_jsonl::*;

#[path = "qwen.rs"]
mod capture_tests_qwen;
pub(crate) use capture_tests_qwen::*;

#[path = "provider_source.rs"]
mod capture_tests_provider_source;
pub(crate) use capture_tests_provider_source::*;

#[path = "session.rs"]
mod capture_tests_session;
pub(crate) use capture_tests_session::*;

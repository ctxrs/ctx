#![allow(dead_code)]
#![allow(unused_imports)]

use assert_cmd::Command;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use predicates::prelude::*;

use ring::{
    rand::SystemRandom,
    signature::{RsaKeyPair, RSA_PKCS1_SHA256},
};

use rusqlite::{params, Connection};

use serde_json::{json, Value};

use sha2::{Digest, Sha256};

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tempfile::{Builder, TempDir};

#[path = "cli/search.rs"]
mod cli_test_search;
pub(crate) use cli_test_search::*;

#[path = "cli/ctx.rs"]
mod cli_test_ctx;
pub(crate) use cli_test_ctx::*;

#[path = "cli/path.rs"]
mod cli_test_path;
pub(crate) use cli_test_path::*;

#[path = "cli/analytics.rs"]
mod cli_test_analytics;
pub(crate) use cli_test_analytics::*;

#[path = "cli/codex_01.rs"]
mod cli_test_codex_01;
pub(crate) use cli_test_codex_01::*;

#[path = "cli/codex_02.rs"]
mod cli_test_codex_02;
pub(crate) use cli_test_codex_02::*;

#[path = "cli/codex_03.rs"]
mod cli_test_codex_03;
pub(crate) use cli_test_codex_03::*;

#[path = "cli/codex_04.rs"]
mod cli_test_codex_04;
pub(crate) use cli_test_codex_04::*;

#[path = "cli/fixture.rs"]
mod cli_test_fixture;
pub(crate) use cli_test_fixture::*;

#[path = "cli/custom_history.rs"]
mod cli_test_custom_history;
pub(crate) use cli_test_custom_history::*;

#[path = "cli/history_source_plugin.rs"]
mod cli_test_history_source_plugin;
pub(crate) use cli_test_history_source_plugin::*;

#[path = "cli/cursor.rs"]
mod cli_test_cursor;
pub(crate) use cli_test_cursor::*;

#[path = "cli/python.rs"]
mod cli_test_python;
pub(crate) use cli_test_python::*;

#[path = "cli/event.rs"]
mod cli_test_event;
pub(crate) use cli_test_event::*;

#[path = "cli/sha256.rs"]
mod cli_test_sha256;
pub(crate) use cli_test_sha256::*;

#[path = "cli/json.rs"]
mod cli_test_json;
pub(crate) use cli_test_json::*;

#[path = "cli/failure.rs"]
mod cli_test_failure;
pub(crate) use cli_test_failure::*;

#[path = "cli/test.rs"]
mod cli_test_test;
pub(crate) use cli_test_test::*;

#[path = "cli/pem.rs"]
mod cli_test_pem;
pub(crate) use cli_test_pem::*;

#[path = "cli/sign.rs"]
mod cli_test_sign;
pub(crate) use cli_test_sign::*;

#[path = "cli/mcp.rs"]
mod cli_test_mcp;
pub(crate) use cli_test_mcp::*;

#[path = "cli/assert.rs"]
mod cli_test_assert;
pub(crate) use cli_test_assert::*;

#[path = "cli/local.rs"]
mod cli_test_local;
pub(crate) use cli_test_local::*;

#[path = "cli/sqlite.rs"]
mod cli_test_sqlite;
pub(crate) use cli_test_sqlite::*;

#[path = "cli/session.rs"]
mod cli_test_session;
pub(crate) use cli_test_session::*;

#[path = "cli/import.rs"]
mod cli_test_import;
pub(crate) use cli_test_import::*;

#[path = "cli/root.rs"]
mod cli_test_root;
pub(crate) use cli_test_root::*;

#[path = "cli/record.rs"]
mod cli_test_record;
pub(crate) use cli_test_record::*;

#[path = "cli/setup.rs"]
mod cli_test_setup;
pub(crate) use cli_test_setup::*;

#[path = "cli/hermes.rs"]
mod cli_test_hermes;
pub(crate) use cli_test_hermes::*;

#[path = "cli/kimi.rs"]
mod cli_test_kimi;
pub(crate) use cli_test_kimi::*;

#[path = "cli/shelley.rs"]
mod cli_test_shelley;
pub(crate) use cli_test_shelley::*;

#[path = "cli/forgecode.rs"]
mod cli_test_forgecode;
pub(crate) use cli_test_forgecode::*;

#[path = "cli/nanoclaw.rs"]
mod cli_test_nanoclaw;
pub(crate) use cli_test_nanoclaw::*;

#[path = "cli/windsurf.rs"]
mod cli_test_windsurf;
pub(crate) use cli_test_windsurf::*;

#[path = "cli/opencode.rs"]
mod cli_test_opencode;
pub(crate) use cli_test_opencode::*;

#[path = "cli/copilot.rs"]
mod cli_test_copilot;
pub(crate) use cli_test_copilot::*;

#[path = "cli/catalog.rs"]
mod cli_test_catalog;
pub(crate) use cli_test_catalog::*;

#[path = "cli/schema.rs"]
mod cli_test_schema;
pub(crate) use cli_test_schema::*;

#[path = "cli/show.rs"]
mod cli_test_show;
pub(crate) use cli_test_show::*;

#[path = "cli/artifact.rs"]
mod cli_test_artifact;
pub(crate) use cli_test_artifact::*;

#[path = "cli/rewrite.rs"]
mod cli_test_rewrite;
pub(crate) use cli_test_rewrite::*;

#[path = "cli/upgrade.rs"]
mod cli_test_upgrade;
pub(crate) use cli_test_upgrade::*;

#[path = "cli/status.rs"]
mod cli_test_status;
pub(crate) use cli_test_status::*;

#[path = "cli/identity.rs"]
mod cli_test_identity;
pub(crate) use cli_test_identity::*;

#[path = "cli/removed.rs"]
mod cli_test_removed;
pub(crate) use cli_test_removed::*;

#[path = "cli/claude.rs"]
mod cli_test_claude;
pub(crate) use cli_test_claude::*;

#[path = "cli/pi.rs"]
mod cli_test_pi;
pub(crate) use cli_test_pi::*;

#[path = "cli/trae.rs"]
mod cli_test_trae;
pub(crate) use cli_test_trae::*;

#[path = "cli/astrbot.rs"]
mod cli_test_astrbot;
pub(crate) use cli_test_astrbot::*;

#[path = "cli/warp.rs"]
mod cli_test_warp;
pub(crate) use cli_test_warp::*;

#[path = "cli/lingma.rs"]
mod cli_test_lingma;
pub(crate) use cli_test_lingma::*;

#[path = "cli/tabnine.rs"]
mod cli_test_tabnine;
pub(crate) use cli_test_tabnine::*;

#[path = "cli/deepagents.rs"]
mod cli_test_deepagents;
pub(crate) use cli_test_deepagents::*;

#[path = "cli/crush.rs"]
mod cli_test_crush;
pub(crate) use cli_test_crush::*;

#[path = "cli/openclaw.rs"]
mod cli_test_openclaw;
pub(crate) use cli_test_openclaw::*;

#[path = "cli/qoder.rs"]
mod cli_test_qoder;
pub(crate) use cli_test_qoder::*;

#[path = "cli/kilo.rs"]
mod cli_test_kilo;
pub(crate) use cli_test_kilo::*;

#[path = "cli/kiro.rs"]
mod cli_test_kiro;
pub(crate) use cli_test_kiro::*;

#[path = "cli/mistral.rs"]
mod cli_test_mistral;
pub(crate) use cli_test_mistral::*;

#[path = "cli/mux.rs"]
mod cli_test_mux;
pub(crate) use cli_test_mux::*;

#[path = "cli/rovodev.rs"]
mod cli_test_rovodev;
pub(crate) use cli_test_rovodev::*;

#[path = "cli/auggie.rs"]
mod cli_test_auggie;
pub(crate) use cli_test_auggie::*;

#[path = "cli/junie.rs"]
mod cli_test_junie;
pub(crate) use cli_test_junie::*;

#[path = "cli/openhands.rs"]
mod cli_test_openhands;
pub(crate) use cli_test_openhands::*;

#[path = "cli/firebender.rs"]
mod cli_test_firebender;
pub(crate) use cli_test_firebender::*;

#[path = "cli/gemini.rs"]
mod cli_test_gemini;
pub(crate) use cli_test_gemini::*;

#[path = "cli/factory.rs"]
mod cli_test_factory;
pub(crate) use cli_test_factory::*;

#[path = "cli/qwen.rs"]
mod cli_test_qwen;
pub(crate) use cli_test_qwen::*;

#[path = "cli/code_buddy.rs"]
mod cli_test_code_buddy;
pub(crate) use cli_test_code_buddy::*;

#[path = "cli/cline.rs"]
mod cli_test_cline;
pub(crate) use cli_test_cline::*;

#[path = "cli/antigravity.rs"]
mod cli_test_antigravity;
pub(crate) use cli_test_antigravity::*;

#![allow(unused_imports)]
use std::{
    collections::HashSet,
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, ProviderRawRetention, ProviderRedactionBoundary};

use rusqlite::{Connection, OpenFlags};

use serde_json::Value;

#[path = "provider_sources/provider_source.rs"]
mod provider_sources_provider_source;
pub(crate) use provider_sources_provider_source::*;
pub use provider_sources_provider_source::{
    discover_provider_sources, discover_provider_sources_for_provider, provider_source_for_path,
    provider_source_spec, provider_source_specs, ProviderDefaultLocation, ProviderSource,
    ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus,
};

#[path = "provider_sources/import.rs"]
mod provider_sources_import;
pub use provider_sources_import::ProviderImportSupport;
pub(crate) use provider_sources_import::*;

#[path = "provider_sources/catalog.rs"]
mod provider_sources_catalog;
pub use provider_sources_catalog::ProviderCatalogSupport;
pub(crate) use provider_sources_catalog::*;

#[path = "provider_sources/codex_01.rs"]
mod provider_sources_codex_01;
pub(crate) use provider_sources_codex_01::*;

#[path = "provider_sources/codex_02.rs"]
mod provider_sources_codex_02;
pub(crate) use provider_sources_codex_02::*;

#[path = "provider_sources/pi.rs"]
mod provider_sources_pi;
pub(crate) use provider_sources_pi::*;

#[path = "provider_sources/claude.rs"]
mod provider_sources_claude;
pub(crate) use provider_sources_claude::*;

#[path = "provider_sources/opencode.rs"]
mod provider_sources_opencode;
pub(crate) use provider_sources_opencode::*;

#[path = "provider_sources/kilo.rs"]
mod provider_sources_kilo;
pub(crate) use provider_sources_kilo::*;

#[path = "provider_sources/kiro.rs"]
mod provider_sources_kiro;
pub(crate) use provider_sources_kiro::*;

#[path = "provider_sources/crush.rs"]
mod provider_sources_crush;
pub(crate) use provider_sources_crush::*;

#[path = "provider_sources/goose.rs"]
mod provider_sources_goose;
pub(crate) use provider_sources_goose::*;

#[path = "provider_sources/warp.rs"]
mod provider_sources_warp;
pub(crate) use provider_sources_warp::*;

#[path = "provider_sources/lingma.rs"]
mod provider_sources_lingma;
pub(crate) use provider_sources_lingma::*;

#[path = "provider_sources/trae.rs"]
mod provider_sources_trae;
pub(crate) use provider_sources_trae::*;

#[path = "provider_sources/qoder.rs"]
mod provider_sources_qoder;
pub(crate) use provider_sources_qoder::*;

#[path = "provider_sources/rovodev.rs"]
mod provider_sources_rovodev;
pub(crate) use provider_sources_rovodev::*;

#[path = "provider_sources/antigravity.rs"]
mod provider_sources_antigravity;
pub(crate) use provider_sources_antigravity::*;

#[path = "provider_sources/gemini.rs"]
mod provider_sources_gemini;
pub(crate) use provider_sources_gemini::*;

#[path = "provider_sources/tabnine.rs"]
mod provider_sources_tabnine;
pub(crate) use provider_sources_tabnine::*;

#[path = "provider_sources/cursor.rs"]
mod provider_sources_cursor;
pub(crate) use provider_sources_cursor::*;

#[path = "provider_sources/windsurf.rs"]
mod provider_sources_windsurf;
pub(crate) use provider_sources_windsurf::*;

#[path = "provider_sources/zed.rs"]
mod provider_sources_zed;
pub(crate) use provider_sources_zed::*;

#[path = "provider_sources/copilot.rs"]
mod provider_sources_copilot;
pub(crate) use provider_sources_copilot::*;

#[path = "provider_sources/factory.rs"]
mod provider_sources_factory;
pub(crate) use provider_sources_factory::*;

#[path = "provider_sources/qwen.rs"]
mod provider_sources_qwen;
pub(crate) use provider_sources_qwen::*;

#[path = "provider_sources/kimi.rs"]
mod provider_sources_kimi;
pub(crate) use provider_sources_kimi::*;

#[path = "provider_sources/auggie.rs"]
mod provider_sources_auggie;
pub(crate) use provider_sources_auggie::*;

#[path = "provider_sources/junie.rs"]
mod provider_sources_junie;
pub(crate) use provider_sources_junie::*;

#[path = "provider_sources/firebender.rs"]
mod provider_sources_firebender;
pub(crate) use provider_sources_firebender::*;

#[path = "provider_sources/forgecode.rs"]
mod provider_sources_forgecode;
pub(crate) use provider_sources_forgecode::*;

#[path = "provider_sources/deepagents.rs"]
mod provider_sources_deepagents;
pub(crate) use provider_sources_deepagents::*;

#[path = "provider_sources/mistral.rs"]
mod provider_sources_mistral;
pub(crate) use provider_sources_mistral::*;

#[path = "provider_sources/mux.rs"]
mod provider_sources_mux;
pub(crate) use provider_sources_mux::*;

#[path = "provider_sources/openclaw.rs"]
mod provider_sources_openclaw;
pub(crate) use provider_sources_openclaw::*;

#[path = "provider_sources/hermes.rs"]
mod provider_sources_hermes;
pub(crate) use provider_sources_hermes::*;

#[path = "provider_sources/nanoclaw.rs"]
mod provider_sources_nanoclaw;
pub(crate) use provider_sources_nanoclaw::*;

#[path = "provider_sources/astrbot.rs"]
mod provider_sources_astrbot;
pub(crate) use provider_sources_astrbot::*;

#[path = "provider_sources/shelley.rs"]
mod provider_sources_shelley;
pub(crate) use provider_sources_shelley::*;

#[path = "provider_sources/openhands.rs"]
mod provider_sources_openhands;
pub(crate) use provider_sources_openhands::*;

#[path = "provider_sources/cline.rs"]
mod provider_sources_cline;
pub(crate) use provider_sources_cline::*;

#[path = "provider_sources/code_buddy.rs"]
mod provider_sources_code_buddy;
pub(crate) use provider_sources_code_buddy::*;

#[path = "provider_sources/env.rs"]
mod provider_sources_env;
pub(crate) use provider_sources_env::*;

#[path = "provider_sources/path.rs"]
mod provider_sources_path;
pub(crate) use provider_sources_path::*;

#[path = "provider_sources/error.rs"]
mod provider_sources_error;
pub(crate) use provider_sources_error::*;

#[path = "provider_sources/bounded.rs"]
mod provider_sources_bounded;
pub(crate) use provider_sources_bounded::*;

#[cfg(test)]
#[path = "provider_sources_tests/tests/mod.rs"]
mod tests;

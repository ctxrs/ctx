#![allow(unused_imports)]
use super::*;

use std::sync::Mutex;

#[path = "env.rs"]
mod provider_sources_tests_env;
pub(crate) use provider_sources_tests_env::*;

#[path = "path.rs"]
mod provider_sources_tests_path;
pub(crate) use provider_sources_tests_path::*;

#[path = "cwd.rs"]
mod provider_sources_tests_cwd;
pub(crate) use provider_sources_tests_cwd::*;

#[path = "gemini.rs"]
mod provider_sources_tests_gemini;
pub(crate) use provider_sources_tests_gemini::*;

#[path = "tabnine.rs"]
mod provider_sources_tests_tabnine;
pub(crate) use provider_sources_tests_tabnine::*;

#[path = "codex.rs"]
mod provider_sources_tests_codex;
pub(crate) use provider_sources_tests_codex::*;

#[path = "pi.rs"]
mod provider_sources_tests_pi;
pub(crate) use provider_sources_tests_pi::*;

#[path = "kilo.rs"]
mod provider_sources_tests_kilo;
pub(crate) use provider_sources_tests_kilo::*;

#[path = "qwen.rs"]
mod provider_sources_tests_qwen;
pub(crate) use provider_sources_tests_qwen::*;

#[path = "kimi.rs"]
mod provider_sources_tests_kimi;
pub(crate) use provider_sources_tests_kimi::*;

#[path = "code_buddy.rs"]
mod provider_sources_tests_code_buddy;
pub(crate) use provider_sources_tests_code_buddy::*;

#[path = "firebender.rs"]
mod provider_sources_tests_firebender;
pub(crate) use provider_sources_tests_firebender::*;

#[path = "junie.rs"]
mod provider_sources_tests_junie;
pub(crate) use provider_sources_tests_junie::*;

#[path = "mistral.rs"]
mod provider_sources_tests_mistral;
pub(crate) use provider_sources_tests_mistral::*;

#[path = "mux.rs"]
mod provider_sources_tests_mux;
pub(crate) use provider_sources_tests_mux::*;

#[path = "deepagents.rs"]
mod provider_sources_tests_deepagents;
pub(crate) use provider_sources_tests_deepagents::*;

#[path = "crush.rs"]
mod provider_sources_tests_crush;
pub(crate) use provider_sources_tests_crush::*;

#[path = "goose.rs"]
mod provider_sources_tests_goose;
pub(crate) use provider_sources_tests_goose::*;

#[path = "warp.rs"]
mod provider_sources_tests_warp;
pub(crate) use provider_sources_tests_warp::*;

#[path = "lingma.rs"]
mod provider_sources_tests_lingma;
pub(crate) use provider_sources_tests_lingma::*;

#[path = "trae.rs"]
mod provider_sources_tests_trae;
pub(crate) use provider_sources_tests_trae::*;

#[path = "cline.rs"]
mod provider_sources_tests_cline;
pub(crate) use provider_sources_tests_cline::*;

#[path = "roo.rs"]
mod provider_sources_tests_roo;
pub(crate) use provider_sources_tests_roo::*;

#[path = "claude.rs"]
mod provider_sources_tests_claude;
pub(crate) use provider_sources_tests_claude::*;

#[path = "fixture.rs"]
mod provider_sources_tests_fixture;
pub(crate) use provider_sources_tests_fixture::*;

#[path = "provider_source.rs"]
mod provider_sources_tests_provider_source;
pub(crate) use provider_sources_tests_provider_source::*;

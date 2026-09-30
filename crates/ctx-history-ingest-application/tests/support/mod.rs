#![allow(dead_code, unused_imports)]

pub(crate) use assert_cmd::Command;
pub(crate) use predicates::prelude::*;
pub(crate) use serde_json::{json, Value};
pub(crate) use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub(crate) use tempfile::{Builder, TempDir};

#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/analytics.rs"]
mod analytics;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/assertions.rs"]
mod assertions;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/daemon.rs"]
mod daemon;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/fixtures.rs"]
mod fixtures;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/history_plugins.rs"]
mod history_plugins;
#[path = "../../../ctx-agent-application/tests/contracts/support/mcp.rs"]
mod mcp;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/native_fixtures/json_tree.rs"]
mod native_directory_fixtures;
mod native_fixtures;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/runner.rs"]
mod runner;

pub(crate) use analytics::*;
pub(crate) use assertions::*;
pub(crate) use daemon::{daemon_test_root, wait_for_test_lexical_projection};
pub(crate) use fixtures::provider_core_counts;
pub(crate) use history_plugins::*;
pub(crate) use mcp::*;
pub(crate) use native_directory_fixtures::{
    write_native_claude_fixture, write_native_cursor_fixture, write_native_gemini_fixture,
    write_native_mistral_vibe_fixture, write_native_mux_fixture,
};
pub(crate) use native_fixtures::*;
pub(crate) use runner::*;

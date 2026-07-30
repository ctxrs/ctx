#![allow(dead_code, unused_imports)]

pub(crate) use assert_cmd::Command;
pub(crate) use predicates::prelude::*;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
pub(crate) use rusqlite::{params, Connection};
pub(crate) use serde_json::{json, Value};
pub(crate) use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub(crate) use tempfile::{Builder, TempDir};

mod analytics;
mod assertions;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
mod daemon;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
mod fixtures;
mod history_plugins;
mod mcp;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
mod native_fixtures;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_pro))]
mod pro;
mod runner;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade))]
mod upgrade;

pub(crate) use analytics::*;
pub(crate) use assertions::*;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
pub(crate) use daemon::*;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
pub(crate) use fixtures::*;
pub(crate) use history_plugins::*;
pub(crate) use mcp::*;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
pub(crate) use native_fixtures::*;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_pro))]
pub(crate) use pro::*;
pub(crate) use runner::*;
#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade))]
pub(crate) use upgrade::*;

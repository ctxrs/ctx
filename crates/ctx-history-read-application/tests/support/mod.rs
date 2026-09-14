#![allow(dead_code, unused_imports)]

pub(crate) use assert_cmd::Command;
pub(crate) use predicates::prelude::*;
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

#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/assertions.rs"]
mod assertions;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/daemon.rs"]
mod daemon;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/fixtures.rs"]
mod fixtures;
#[path = "../../../ctx-agent-application/tests/contracts/support/mcp.rs"]
mod mcp;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/native_fixtures.rs"]
mod native_fixtures;
#[path = "../../../ctx-cli-contract-tests/tests/contracts/support/runner.rs"]
mod runner;

pub(crate) use assertions::*;
pub(crate) use daemon::*;
pub(crate) use fixtures::*;
pub(crate) use mcp::*;
pub(crate) use native_fixtures::*;
pub(crate) use runner::*;

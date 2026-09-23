//! Explicit graph hooks. Managed skills and default MCP setup belong to ctx integrations.
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::switch_files;

mod files;
mod git_hooks;
mod tool_hooks;
use files::*;
pub use git_hooks::hook;
use git_hooks::quote;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Agent host for explicit tool hooks; skills and MCP use ctx integrations.
    #[arg(long, default_value = "agents")]
    pub platform: String,
    /// Project directory (defaults to the current directory).
    #[arg(long, conflicts_with = "global")]
    pub project: Option<PathBuf>,
    /// Explicitly select the user-global installation.
    #[arg(long)]
    pub global: bool,
    /// Existing Claude, Codex or Hermes configuration root; also select it in the host.
    #[arg(long, requires = "global", conflicts_with = "profile")]
    pub config_root: Option<PathBuf>,
    /// Existing VS Code user-profile directory (locate via MCP: Open User Configuration).
    #[arg(long, requires = "global", conflicts_with = "config_root")]
    pub profile: Option<PathBuf>,
    /// Use ctx integrations install mcp for the managed ctx MCP server.
    #[arg(long)]
    pub mcp: bool,
    /// Use ctx integrations install skill for managed guidance.
    #[arg(long)]
    pub skill: bool,
    /// Opt in to fail-open source read/search guidance (Claude, CodeBuddy, Gemini projects).
    #[arg(long)]
    pub tool_hooks: bool,
}

#[derive(Debug, Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub command: HookCommand,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Opt in to foreground graph refresh after commits, checkouts, and merges.
    Install {
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Restore receipt-owned hooks; refuse later edits.
    Uninstall {
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Inspect hook installation without modifying files.
    Status {
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub status: String,
    pub platform: String,
    pub scope: PathBuf,
    pub files: Vec<PathBuf>,
    pub notes: Vec<String>,
}

const LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
    /// A hook backup retains the original access permissions, including ACLs.
    permission_source: Option<PathBuf>,
    executable: bool,
    /// Previous installed bytes accepted only while a guidance upgrade is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_after: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    scope: PathBuf,
    changes: Vec<Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guidance_version: Option<u32>,
}

fn root(path: Option<&Path>) -> Result<PathBuf> {
    let path = path.unwrap_or(Path::new(".")).canonicalize()?;
    ensure!(path.is_dir(), "scope must be a directory");
    Ok(path)
}

// Skills and the default MCP connection have one owner: ctx integrations.
pub fn install(args: &SetupArgs) -> Result<SetupReport> {
    tool_hooks::setup(args, false)
}
pub fn uninstall(args: &SetupArgs) -> Result<SetupReport> {
    tool_hooks::setup(args, true)
}

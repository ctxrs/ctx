#![allow(unused_imports)]

use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{analytics, AnalyticsProperties};

const BUNDLED_SKILL_NAME: &str = "ctx-agent-history-search";
const BUNDLED_SKILL_BODY: &str = include_str!("../../../skills/ctx-agent-history-search/SKILL.md");
const METADATA_FILE: &str = ".ctx-skill.json";

mod args;
mod install;
mod selection;

pub(crate) use args::*;
pub(crate) use install::*;
pub(crate) use selection::*;

#[cfg(test)]
mod tests;

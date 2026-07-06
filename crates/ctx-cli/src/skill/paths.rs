use std::{
    collections::BTreeMap,
    env,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use super::{agents::SkillAgentArg, BUNDLED_SKILL_BODY};

#[derive(Debug, Clone)]
pub(super) struct PathContext {
    pub(super) home: PathBuf,
    pub(super) xdg_config_home: PathBuf,
    pub(super) cwd: PathBuf,
    pub(super) env_overrides: BTreeMap<String, PathBuf>,
}

impl PathContext {
    pub(super) fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let mut env_overrides = BTreeMap::new();
        for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
            if let Some(path) = non_empty_env_path(key) {
                env_overrides.insert(key.to_owned(), path);
            }
        }
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            env_overrides,
        })
    }

    #[cfg(test)]
    pub(super) fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            env_overrides: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_env_override(mut self, key: &str, value: PathBuf) -> Self {
        self.env_overrides.insert(key.to_owned(), value);
        self
    }

    #[cfg(test)]
    pub(super) fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    pub(super) fn env_or_home_child(&self, key: &str, fallback_child: &str) -> PathBuf {
        self.env_overrides
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.home.join(fallback_child))
    }

    pub(super) fn agent_detected(&self, agent: SkillAgentArg) -> bool {
        if agent == SkillAgentArg::Codex
            && !self.env_overrides.contains_key("CODEX_HOME")
            && Path::new("/etc/codex").exists()
        {
            return true;
        }
        agent.detect_dir(self).is_some_and(|path| path.exists())
    }
}

fn home_dir() -> Option<PathBuf> {
    non_empty_env_path("HOME").or_else(|| non_empty_env_path("USERPROFILE"))
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(super) fn sanitize_skill_name(name: &str) -> Result<String> {
    let mut sanitized = String::with_capacity(name.len());
    let mut previous_dash = false;
    for ch in name.trim().chars().flat_map(char::to_lowercase) {
        let allowed = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_';
        if allowed {
            sanitized.push(ch);
            previous_dash = false;
        } else if !previous_dash {
            sanitized.push('-');
            previous_dash = true;
        }
    }
    let sanitized = sanitized
        .trim_matches(|ch| ch == '.' || ch == '-')
        .chars()
        .take(255)
        .collect::<String>();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err(anyhow!("invalid skill name"));
    }
    Ok(sanitized)
}

pub(super) fn ensure_path_inside(base: &Path, target: &Path) -> Result<()> {
    if has_parent_component(base) || has_parent_component(target) {
        return Err(anyhow!("skill path contains parent traversal"));
    }
    if !target.starts_with(base) {
        return Err(anyhow!("skill path escapes target directory"));
    }
    Ok(())
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

pub(super) fn bundled_hash() -> String {
    sha256_hex(BUNDLED_SKILL_BODY.as_bytes())
}

pub(super) fn sha256_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("sha256:{:x}", hasher.finalize())
}

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use super::{agents::SkillAgentArg, BUNDLED_SKILL_BODY};

#[derive(Debug, Clone)]
pub struct PathContext {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
    pub cwd: PathBuf,
    pub env_overrides: BTreeMap<String, PathBuf>,
}

impl PathContext {
    pub fn from_env() -> Result<Self> {
        Self::from_env_with_agent_override_policy(false)
    }

    pub fn from_env_best_effort() -> Result<Self> {
        Self::from_env_with_agent_override_policy(true)
    }

    fn from_env_with_agent_override_policy(ignore_invalid_agent_overrides: bool) -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let mut env_overrides = BTreeMap::new();
        for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
            if let Some(path) = non_empty_env_path(key) {
                env_overrides.insert(key.to_owned(), path);
            }
        }
        if let Some(path) = agent_home_override(
            non_empty_absolute_env_path("MIMOCODE_HOME"),
            ignore_invalid_agent_overrides,
        )? {
            env_overrides.insert("MIMOCODE_HOME".to_owned(), path);
        }
        if let Some(path) = agent_home_override(
            absolute_env_path_if_present("GROK_HOME"),
            ignore_invalid_agent_overrides,
        )? {
            env_overrides.insert("GROK_HOME".to_owned(), path);
        }
        if let Some(path) = non_empty_env_path("MIMOCODE_CONFIG_DIR") {
            env_overrides.insert("MIMOCODE_CONFIG_DIR".to_owned(), path);
        }
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            env_overrides,
        })
    }

    pub fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            env_overrides: BTreeMap::new(),
        }
    }

    pub fn with_env_override(mut self, key: &str, value: PathBuf) -> Self {
        self.env_overrides.insert(key.to_owned(), value);
        self
    }

    pub fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    pub fn env_or_home_child(&self, key: &str, fallback_child: &str) -> PathBuf {
        self.env_overrides
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.home.join(fallback_child))
    }

    pub fn mimocode_config_dir(&self) -> PathBuf {
        if let Some(path) = self.env_overrides.get("MIMOCODE_CONFIG_DIR") {
            return path.clone();
        }
        self.env_overrides
            .get("MIMOCODE_HOME")
            .map(|home| home.join("config"))
            .unwrap_or_else(|| self.xdg_config_home.join("mimocode"))
    }

    pub fn agent_detected(&self, agent: SkillAgentArg) -> bool {
        if agent == SkillAgentArg::Codex
            && !self.env_overrides.contains_key("CODEX_HOME")
            && Path::new("/etc/codex").exists()
        {
            return true;
        }
        if agent == SkillAgentArg::MiMoCode
            && (self.env_overrides.contains_key("MIMOCODE_HOME")
                || self.env_overrides.contains_key("MIMOCODE_CONFIG_DIR"))
        {
            return true;
        }
        if agent == SkillAgentArg::GrokBuild && self.env_overrides.contains_key("GROK_HOME") {
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

fn non_empty_absolute_env_path(key: &str) -> Result<Option<PathBuf>> {
    let Some(path) = non_empty_env_path(key) else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err(anyhow!(
            "{key} must be an absolute path: {}",
            path.display()
        ));
    }
    Ok(Some(path))
}

fn absolute_env_path_if_present(key: &str) -> Result<Option<PathBuf>> {
    validate_absolute_env_path(key, env::var_os(key))
}

fn agent_home_override(
    override_path: Result<Option<PathBuf>>,
    ignore_invalid: bool,
) -> Result<Option<PathBuf>> {
    match override_path {
        Err(_) if ignore_invalid => Ok(None),
        result => result,
    }
}

fn validate_absolute_env_path(key: &str, value: Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(anyhow!("{key} must be nonempty and absolute"));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(anyhow!(
            "{key} must be an absolute path: {}",
            path.display()
        ));
    }
    Ok(Some(path))
}

pub fn sanitize_skill_name(name: &str) -> Result<String> {
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

pub fn ensure_path_inside(base: &Path, target: &Path) -> Result<()> {
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

pub fn bundled_hash() -> String {
    sha256_hex(BUNDLED_SKILL_BODY.as_bytes())
}

pub fn sha256_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod env_path_tests {
    use super::*;

    #[test]
    fn grok_home_contract_rejects_empty_and_relative_values() {
        assert!(validate_absolute_env_path("GROK_HOME", Some(OsString::new())).is_err());
        assert!(validate_absolute_env_path("GROK_HOME", Some("relative".into())).is_err());
        assert_eq!(
            validate_absolute_env_path("GROK_HOME", Some("/grok-home".into())).unwrap(),
            Some(PathBuf::from("/grok-home"))
        );
        assert_eq!(validate_absolute_env_path("GROK_HOME", None).unwrap(), None);
    }

    #[test]
    fn best_effort_policy_ignores_only_invalid_agent_home_overrides() {
        let invalid = || validate_absolute_env_path("GROK_HOME", Some("relative".into()));
        assert!(agent_home_override(invalid(), false).is_err());
        assert_eq!(agent_home_override(invalid(), true).unwrap(), None);
        assert_eq!(
            agent_home_override(
                validate_absolute_env_path("GROK_HOME", Some("/grok-home".into())),
                true,
            )
            .unwrap(),
            Some(PathBuf::from("/grok-home"))
        );
    }
}

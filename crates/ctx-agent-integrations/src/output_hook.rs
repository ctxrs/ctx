//! Explicit native output-hook registration. The host's permission settings and
//! unrelated hooks are never changed; runtime transformation lives in ctx-output.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, ensure, Context as _, Result};
use serde_json::{json, Value};

use crate::{
    filesystem::atomic_update,
    mcp_config::format::json,
    skill::{PathContext, SkillAgentArg},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Agent {
    Claude,
    Copilot,
    Codex,
}

impl Agent {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Copilot, Self::Codex];

    pub fn from_skill(agent: SkillAgentArg) -> Option<Self> {
        match agent {
            SkillAgentArg::ClaudeCode => Some(Self::Claude),
            SkillAgentArg::GitHubCopilot => Some(Self::Copilot),
            SkillAgentArg::Codex => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Copilot => "copilot",
            Self::Codex => "codex",
        }
    }

    pub fn limitation(self) -> &'static str {
        match self {
            Self::Claude => "requires Claude Code >=2.1.121; host version is not probed",
            Self::Copilot => "Copilot CLI postToolUse only; VS Code replacement is not available",
            Self::Codex => "POSIX only; Codex hook support and /hooks trust review are required",
        }
    }

    pub fn supported(self) -> bool {
        self != Self::Codex || cfg!(unix)
    }

    fn event(self) -> &'static str {
        match self {
            Self::Claude => "PostToolUse",
            Self::Copilot => "postToolUse",
            Self::Codex => "PreToolUse",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Context {
    pub paths: PathContext,
    pub executable: PathBuf,
}

impl Context {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            paths: PathContext::from_env()?,
            executable: std::env::current_exe()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Missing,
    Current,
    SiftConflict,
    Conflict,
    Unsupported,
}

fn path(agent: Agent, project: bool, context: &Context) -> PathBuf {
    let paths = &context.paths;
    let root = match (agent, project) {
        (Agent::Claude, true) => paths.cwd.join(".claude"),
        (Agent::Claude, false) => paths.env_or_home_child("CLAUDE_CONFIG_DIR", ".claude"),
        (Agent::Copilot, true) => paths.cwd.join(".github"),
        (Agent::Copilot, false) => paths.env_or_home_child("COPILOT_HOME", ".copilot"),
        (Agent::Codex, true) => paths.cwd.join(".codex"),
        (Agent::Codex, false) => paths.env_or_home_child("CODEX_HOME", ".codex"),
    };
    match agent {
        Agent::Claude => root.join("settings.json"),
        Agent::Copilot => root.join("hooks/ctx.json"),
        Agent::Codex => root.join("hooks.json"),
    }
}

fn document(path: &Path) -> Result<Option<Value>> {
    let body = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let value = json::parse(&body, path)?;
    ensure!(
        value.is_object(),
        "hook config root must be an object: {}",
        path.display()
    );
    Ok(Some(value))
}

fn expected(agent: Agent, executable: &Path) -> Result<Value> {
    ensure!(
        executable.is_absolute(),
        "ctx hook executable must be absolute"
    );
    let path = executable
        .to_str()
        .context("ctx hook executable must be UTF-8")?;
    if agent == Agent::Copilot {
        return Ok(
            json!({"type":"command", "matcher":"bash|powershell", "exec":path,
            "args":["sift","hook","copilot"], "timeoutSec":10}),
        );
    }
    let command = if cfg!(windows) {
        format!(
            "& '{}' sift hook {}",
            path.replace('\'', "''"),
            agent.name()
        )
    } else {
        format!(
            "'{}' sift hook {}",
            path.replace('\'', "'\\''"),
            agent.name()
        )
    };
    let hook = if cfg!(windows) {
        json!({"type":"command", "command":command, "shell":"powershell"})
    } else {
        json!({"type":"command", "command":command})
    };
    Ok(
        json!({"matcher": if agent == Agent::Claude { "Bash|PowerShell" } else { "Bash" },
        "hooks":[hook]}),
    )
}

fn hook_entries<'a>(value: &'a Value, event: &str) -> Result<Option<&'a Vec<Value>>> {
    let Some(hooks) = value.get("hooks") else {
        return Ok(None);
    };
    let hooks = hooks.as_object().context("hooks must be an object")?;
    hooks
        .get(event)
        .map(|v| v.as_array().context("hook event must be an array"))
        .transpose()
}

fn hook_entries_mut<'a>(value: &'a mut Value, event: &str) -> Result<&'a mut Vec<Value>> {
    let root = value
        .as_object_mut()
        .context("hook config root must be an object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().context("hooks must be an object")?;
    hooks
        .entry(event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("hook event must be an array")
}

fn invocation(value: &Value, name: &str, agent: Agent) -> bool {
    let suffixes = if name == "ctx" {
        vec![
            format!(" sift hook {}", agent.name()),
            format!(" output hook {}", agent.name()),
        ]
    } else {
        vec![format!(" hook {}", agent.name())]
    };
    let shell_command_matches = |v: &str| {
        suffixes.iter().any(|suffix| {
            v.strip_suffix(suffix).is_some_and(|prefix| {
                let prefix = prefix.trim_start_matches("& ").trim();
                let unescaped = if cfg!(windows) {
                    prefix.replace("''", "'")
                } else {
                    prefix.replace("'\\''", "'")
                };
                Path::new(unescaped.trim_matches('\''))
                    .file_stem()
                    .is_some_and(|stem| stem == name)
            })
        })
    };
    if ["command", "bash", "powershell", "windows", "linux", "osx"]
        .iter()
        .any(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(&shell_command_matches)
        })
    {
        return true;
    }
    let exec = value.get("exec").and_then(Value::as_str);
    exec.is_some_and(|v| Path::new(v).file_stem().is_some_and(|stem| stem == name))
        && if name == "ctx" {
            matches!(
                value.get("args"),
                Some(args)
                    if args == &json!(["sift", "hook", agent.name()])
                        || args == &json!(["output", "hook", agent.name()])
            )
        } else {
            value.get("args") == Some(&json!(["hook", agent.name()]))
        }
}

fn contains_invocation(value: &Value, name: &str, agent: Agent) -> bool {
    invocation(value, name, agent)
        || match value {
            Value::Array(items) => items.iter().any(|v| contains_invocation(v, name, agent)),
            Value::Object(fields) => fields.values().any(|v| contains_invocation(v, name, agent)),
            _ => false,
        }
}

fn count_invocations(value: &Value, name: &str, agent: Agent) -> usize {
    usize::from(invocation(value, name, agent))
        + match value {
            Value::Array(items) => items
                .iter()
                .map(|v| count_invocations(v, name, agent))
                .sum(),
            Value::Object(fields) => fields
                .values()
                .map(|v| count_invocations(v, name, agent))
                .sum(),
            _ => 0,
        }
}

fn owned_entry(entry: &Value, agent: Agent) -> bool {
    let path = (|| {
        if agent == Agent::Copilot {
            return Some(PathBuf::from(entry.get("exec")?.as_str()?));
        }
        let command = entry
            .get("hooks")?
            .as_array()?
            .first()?
            .get("command")?
            .as_str()?;
        let (prefix, _) = [
            format!(" sift hook {}", agent.name()),
            format!(" output hook {}", agent.name()),
        ]
        .iter()
        .find_map(|suffix| command.strip_suffix(suffix).map(|prefix| (prefix, suffix)))?;
        if cfg!(windows) {
            Some(PathBuf::from(
                prefix
                    .strip_prefix("& '")?
                    .strip_suffix('\'')?
                    .replace("''", "'"),
            ))
        } else {
            Some(PathBuf::from(
                prefix
                    .strip_prefix('\'')?
                    .strip_suffix('\'')?
                    .replace("'\\''", "'"),
            ))
        }
    })();
    path.is_some_and(|path| expected(agent, &path).is_ok_and(|desired| desired == *entry))
}

fn copilot_hook_sources(context: &Context) -> Result<Vec<PathBuf>> {
    let mut sources = vec![context
        .paths
        .env_or_home_child("COPILOT_HOME", ".copilot")
        .join("settings.json")];
    for root in [
        context
            .paths
            .env_or_home_child("COPILOT_HOME", ".copilot")
            .join("hooks"),
        context.paths.cwd.join(".github/hooks"),
    ] {
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("inspect {}", root.display())),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                sources.push(path);
            }
        }
    }
    for root in [
        context.paths.cwd.join(".github/copilot"),
        context.paths.cwd.join(".claude"),
    ] {
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("inspect {}", root.display())),
        };
        for entry in entries {
            let path = entry?.path();
            if path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("settings") && name.ends_with(".json")
            }) {
                sources.push(path);
            }
        }
    }
    Ok(sources)
}

fn sift_conflict(agent: Agent, context: &Context) -> Result<bool> {
    if agent == Agent::Copilot {
        for source in copilot_hook_sources(context)? {
            if let Some(value) = document(&source)? {
                if value.get("hooks").is_some_and(|hooks| {
                    contains_invocation(hooks, "sift", Agent::Copilot)
                        || contains_invocation(hooks, "sift", Agent::Claude)
                }) {
                    return Ok(true);
                }
            }
        }
        return Ok(false);
    }
    for scope in [false, true] {
        let config = path(agent, scope, context);
        if let Some(value) = document(&config)? {
            if value
                .get("hooks")
                .is_some_and(|hooks| contains_invocation(hooks, "sift", agent))
            {
                return Ok(true);
            }
        }
        if agent == Agent::Claude && scope {
            let local = config.with_file_name("settings.local.json");
            if let Some(value) = document(&local)? {
                if value
                    .get("hooks")
                    .is_some_and(|hooks| contains_invocation(hooks, "sift", agent))
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn local_state(agent: Agent, value: Option<&Value>, desired: &Value) -> Result<State> {
    let Some(value) = value else {
        return Ok(State::Missing);
    };
    let current = hook_entries(value, agent.event())?
        .map(|entries| entries.iter().filter(|entry| *entry == desired).count())
        .unwrap_or(0);
    let total = value
        .get("hooks")
        .map(|hooks| count_invocations(hooks, "ctx", agent))
        .unwrap_or(0);
    if current == 1 && total == 1 {
        return Ok(State::Current);
    }
    if current > 1 || total > 0 {
        return Ok(State::Conflict);
    }
    Ok(State::Missing)
}

pub fn status(agent: Agent, project: bool, context: &Context) -> Result<State> {
    if !agent.supported() {
        return Ok(State::Unsupported);
    }
    let target = path(agent, project, context);
    let value = document(&target)?;
    if agent == Agent::Copilot
        && value
            .as_ref()
            .and_then(|v| v.get("version"))
            .is_some_and(|v| v != &json!(1))
    {
        return Ok(State::Conflict);
    }
    let state = local_state(
        agent,
        value.as_ref(),
        &expected(agent, &context.executable)?,
    )?;
    if sift_conflict(agent, context)? {
        return Ok(State::SiftConflict);
    }
    Ok(state)
}

pub fn install(agent: Agent, project: bool, context: &Context) -> Result<State> {
    let state = status(agent, project, context)?;
    match state {
        State::Current => return Ok(state),
        State::Unsupported => bail!("{} Sift hook is unsupported here: {}", agent.name(), agent.limitation()),
        State::SiftConflict => bail!("standalone Sift hook detected; remove it explicitly before installing the ctx Sift hook"),
        State::Conflict => bail!("existing ctx Sift hook differs; inspect it manually before installing"),
        State::Missing => {}
    }
    let target = path(agent, project, context);
    let desired = expected(agent, &context.executable)?;
    atomic_update(&target, |body| {
        let mut value = match body {
            Some(bytes) => json::parse(std::str::from_utf8(bytes)?, &target)?,
            None => json!({}),
        };
        ensure!(
            !value
                .get("hooks")
                .is_some_and(|hooks| contains_invocation(hooks, "sift", agent)),
            "standalone Sift hook appeared during installation; no changes made"
        );
        ensure!(
            local_state(agent, Some(&value), &desired)? == State::Missing,
            "hook config changed; retry after inspecting it"
        );
        if agent == Agent::Copilot {
            let root = value
                .as_object_mut()
                .context("hook config root must be an object")?;
            ensure!(
                root.get("version").is_none_or(|v| v == &json!(1)),
                "unsupported Copilot hook version"
            );
            root.insert("version".into(), json!(1));
        }
        hook_entries_mut(&mut value, agent.event())?.push(desired);
        Ok(json::render(&value)?.into_bytes())
    })?;
    Ok(State::Current)
}

pub fn remove(agent: Agent, project: bool, context: &Context) -> Result<State> {
    if !agent.supported() {
        return Ok(State::Unsupported);
    }
    let target = path(agent, project, context);
    let Some(value) = document(&target)? else {
        return Ok(State::Missing);
    };
    if !hook_entries(&value, agent.event())?
        .is_some_and(|items| items.iter().any(|entry| owned_entry(entry, agent)))
    {
        return Ok(State::Missing);
    }
    atomic_update(&target, |body| {
        let bytes = body.ok_or_else(|| anyhow!("hook config disappeared; retry"))?;
        let mut value = json::parse(std::str::from_utf8(bytes)?, &target)?;
        let entries = hook_entries_mut(&mut value, agent.event())?;
        let before = entries.len();
        entries.retain(|entry| !owned_entry(entry, agent));
        ensure!(
            before > entries.len(),
            "hook config changed; retry after inspecting it"
        );
        Ok(json::render(&value)?.into_bytes())
    })?;
    Ok(State::Missing)
}

#[cfg(test)]
mod tests;

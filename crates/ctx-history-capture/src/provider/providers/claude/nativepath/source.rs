use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{CaptureError, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeSessionKey {
    pub(crate) root_session_id: String,
    pub(crate) workflow_run_id: Option<String>,
    pub(crate) agent_id: Option<String>,
}

impl ClaudeSessionKey {
    pub(crate) fn provider_session_id(&self) -> String {
        match (&self.workflow_run_id, &self.agent_id) {
            (None, None) => self.root_session_id.clone(),
            (None, Some(agent_id)) => {
                format!("{}/subagents/{agent_id}", self.root_session_id)
            }
            (Some(run_id), Some(agent_id)) => format!(
                "{}/subagents/workflows/{run_id}/{agent_id}",
                self.root_session_id
            ),
            (Some(_), None) => self.root_session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionLayout {
    Primary,
    Subagent,
    WorkflowSubagent,
}

pub(crate) fn claude_projects_root(root: &Path) -> PathBuf {
    if root.file_name() == Some(OsStr::new(".claude")) {
        root.join("projects")
    } else {
        root.to_path_buf()
    }
}

pub(crate) fn classify_claude_path(
    projects_root: &Path,
    path: &Path,
) -> Result<Option<(PathBuf, SessionLayout, ClaudeSessionKey)>> {
    let Ok(relative) = path.strip_prefix(projects_root) else {
        return Ok(None);
    };
    let components = relative
        .components()
        .map(|component| component.as_os_str())
        .collect::<Vec<_>>();
    let projects_container = projects_root.file_name() == Some(OsStr::new("projects"));
    let (project_dir, inner) = if projects_container {
        let Some(project) = components.first() else {
            return Ok(None);
        };
        (projects_root.join(project), &components[1..])
    } else {
        (projects_root.to_path_buf(), components.as_slice())
    };
    if inner.len() == 1 && is_jsonl(path) {
        return Ok(Some((
            project_dir,
            SessionLayout::Primary,
            ClaudeSessionKey {
                root_session_id: utf8_file_stem(path)?.to_owned(),
                workflow_run_id: None,
                agent_id: None,
            },
        )));
    }
    if inner.len() == 3 && inner[1] == OsStr::new("subagents") && is_subagent_jsonl(path) {
        return Ok(Some((
            project_dir,
            SessionLayout::Subagent,
            ClaudeSessionKey {
                root_session_id: utf8_component(inner[0], path, "session directory")?.to_owned(),
                workflow_run_id: None,
                agent_id: Some(utf8_file_stem(path)?.to_owned()),
            },
        )));
    }
    if inner.len() == 5
        && inner[1] == OsStr::new("subagents")
        && inner[2] == OsStr::new("workflows")
        && is_subagent_jsonl(path)
    {
        return Ok(Some((
            project_dir,
            SessionLayout::WorkflowSubagent,
            ClaudeSessionKey {
                root_session_id: utf8_component(inner[0], path, "session directory")?.to_owned(),
                workflow_run_id: Some(
                    utf8_component(inner[3], path, "workflow directory")?.to_owned(),
                ),
                agent_id: Some(utf8_file_stem(path)?.to_owned()),
            },
        )));
    }
    Ok(None)
}

fn utf8_file_stem(path: &Path) -> Result<&str> {
    path.file_stem()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Claude session filename is not valid UTF-8",
        })
}

fn utf8_component<'a>(value: &'a OsStr, path: &Path, name: &'static str) -> Result<&'a str> {
    value
        .to_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: match name {
                "workflow directory" => "Claude workflow directory is not valid UTF-8",
                _ => "Claude session directory is not valid UTF-8",
            },
        })
}

fn is_jsonl(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("jsonl"))
}

fn is_subagent_jsonl(path: &Path) -> bool {
    is_jsonl(path)
        && path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("agent-") && name.len() > "agent-".len())
}

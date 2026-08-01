use std::{
    fs::Metadata,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::AgentType;
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{
    provider_capped_json, provider_local_preview, provider_timestamp_seconds_to_datetime,
};
use crate::{fnv1a64, Result, PROVIDER_MAX_PREVIEW_CHARS};

use super::layout::KimiWireLayout;

const KIMI_SESSION_METADATA_TEXT_MAX_CHARS: usize = 16_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KimiWireSessionState {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) agent_id: String,
    pub(super) is_primary: bool,
    pub(super) started_at: Option<DateTime<Utc>>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) state_metadata: Value,
    pub(super) agent_state_metadata: Value,
    pub(super) index_metadata: Option<Value>,
    pub(super) title: Option<String>,
    pub(super) last_prompt: Option<String>,
    pub(super) archived: Option<bool>,
    pub(super) auxiliary_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KimiWireObservation {
    #[cfg(test)]
    layout: KimiWireLayout,
    pub(super) session: KimiWireSessionState,
}

impl KimiWireObservation {
    #[cfg(test)]
    pub(super) fn read(path: &Path) -> Result<Self> {
        Self::from_layout(KimiWireLayout::read(path)?)
    }

    pub(super) fn read_from_admitted(
        path: &Path,
        canonical_wire_path: PathBuf,
        wire_metadata: &Metadata,
        state: Option<(&Metadata, &[u8])>,
        index: Option<(&Metadata, &[u8])>,
    ) -> Result<Self> {
        Self::from_layout(KimiWireLayout::read_from_admitted(
            path,
            canonical_wire_path,
            wire_metadata,
            state,
            index,
        )?)
    }

    fn from_layout(mut layout: KimiWireLayout) -> Result<Self> {
        let state = layout.take_state();
        let index_entry = layout.take_index_entry();
        let agent_id = layout.agent_id().to_owned();
        let session_id = layout.session_id().to_owned();
        let agent_state = state
            .get("agents")
            .and_then(|agents| agents.get(&agent_id))
            .cloned()
            .unwrap_or(Value::Null);
        let (provider_session_id, parent_provider_session_id, root_provider_session_id, _) =
            kimi_provider_session_ids(&session_id, &agent_id, &agent_state);
        let cwd = index_entry
            .as_ref()
            .and_then(|entry| entry.work_dir.clone())
            .or_else(|| {
                state
                    .get("workDir")
                    .or_else(|| state.get("cwd"))
                    .and_then(Value::as_str)
                    .filter(|cwd| !cwd.trim().is_empty())
                    .map(|cwd| capped_kimi_text(cwd, KIMI_SESSION_METADATA_TEXT_MAX_CHARS))
            });
        let state_metadata = provider_capped_json(&state, PROVIDER_MAX_PREVIEW_CHARS);
        let agent_state_metadata = provider_capped_json(&agent_state, PROVIDER_MAX_PREVIEW_CHARS);
        let index_metadata = index_entry.as_ref().map(|entry| entry.metadata());
        let title = state
            .get("title")
            .or_else(|| state.get("customTitle"))
            .and_then(Value::as_str)
            .map(|title| capped_kimi_text(title, KIMI_SESSION_METADATA_TEXT_MAX_CHARS));
        let last_prompt = state
            .get("lastPrompt")
            .and_then(Value::as_str)
            .map(|prompt| capped_kimi_text(prompt, KIMI_SESSION_METADATA_TEXT_MAX_CHARS));
        let started_at = kimi_state_timestamp(&state, &["createdAt", "created_at"]);
        let ended_at = kimi_state_timestamp(&state, &["updatedAt", "updated_at"]);
        let auxiliary_revision = fnv1a64(
            serde_json::to_string(&json!({
                "state": state_metadata,
                "agent_state": agent_state_metadata,
                "index": index_metadata,
                "provider_session_id": provider_session_id,
                "parent_provider_session_id": parent_provider_session_id,
                "root_provider_session_id": root_provider_session_id,
                "cwd": cwd,
                "started_at": started_at,
                "ended_at": ended_at,
                "title": title,
                "last_prompt": last_prompt,
                "archived": state.get("archived").and_then(Value::as_bool),
            }))?
            .as_bytes(),
        );
        let session = KimiWireSessionState {
            provider_session_id,
            parent_provider_session_id,
            root_provider_session_id,
            agent_id: agent_id.clone(),
            is_primary: agent_id == "main",
            started_at,
            ended_at,
            cwd,
            state_metadata,
            agent_state_metadata,
            index_metadata,
            title,
            last_prompt,
            archived: state.get("archived").and_then(Value::as_bool),
            auxiliary_revision,
        };
        Ok(Self {
            #[cfg(test)]
            layout,
            session,
        })
    }

    #[cfg(test)]
    pub(super) fn revalidate(&self, path: &Path) -> Result<bool> {
        self.layout.revalidate(path)
    }
}

pub(crate) fn kimi_state_timestamp(value: &Value, fields: &[&str]) -> Option<DateTime<Utc>> {
    fields.iter().find_map(|field| {
        value.get(*field).and_then(|timestamp| match timestamp {
            Value::String(raw) => parse_rfc3339_utc(raw).or_else(|| {
                raw.parse::<f64>()
                    .ok()
                    .and_then(provider_timestamp_seconds_to_datetime)
            }),
            Value::Number(number) => number
                .as_f64()
                .and_then(provider_timestamp_seconds_to_datetime),
            _ => None,
        })
    })
}

pub(crate) fn kimi_provider_session_ids(
    session_id: &str,
    agent_id: &str,
    agent_state: &Value,
) -> (String, Option<String>, Option<String>, AgentType) {
    if agent_id == "main" {
        return (session_id.to_owned(), None, None, AgentType::Primary);
    }
    let provider_session_id = format!("{session_id}/agents/{agent_id}");
    let parent = agent_state
        .get("parentAgentId")
        .or_else(|| agent_state.get("parent_agent_id"))
        .and_then(Value::as_str)
        .filter(|parent| !parent.trim().is_empty())
        .map(|parent| {
            if parent == "main" {
                session_id.to_owned()
            } else {
                format!("{session_id}/agents/{parent}")
            }
        })
        .or_else(|| Some(session_id.to_owned()));
    (
        provider_session_id,
        parent,
        Some(session_id.to_owned()),
        AgentType::Subagent,
    )
}

fn capped_kimi_text(value: &str, max_chars: usize) -> String {
    provider_local_preview(value, max_chars).0
}

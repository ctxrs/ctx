use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use serde_json::Value;

use crate::app_server::{AppServerClient, ThreadTokenUsage};

use super::canonical_context_window_from_thread_usage;

#[derive(Default)]
pub(super) struct ReasoningSummaryState {
    pub(super) text: String,
}

pub(super) struct TurnRuntimeState {
    pub(super) message_id: Option<String>,
    pub(super) emitted_final: bool,
    pub(super) reasoning_summaries: HashMap<(String, i64), ReasoningSummaryState>,
    pub(super) reasoning_text_seen: HashSet<String>,
}

impl TurnRuntimeState {
    fn new() -> Self {
        Self {
            message_id: None,
            emitted_final: false,
            reasoning_summaries: HashMap::new(),
            reasoning_text_seen: HashSet::new(),
        }
    }
}

pub(super) struct TurnTracker {
    pub(super) session_id: String,
    pub(super) turns: HashMap<String, TurnRuntimeState>,
}

impl TurnTracker {
    pub(super) fn new(session_id: String) -> Self {
        Self {
            session_id,
            turns: HashMap::new(),
        }
    }

    pub(super) fn ensure_turn(&mut self, turn_id: &str) -> &mut TurnRuntimeState {
        self.turns
            .entry(turn_id.to_string())
            .or_insert_with(TurnRuntimeState::new)
    }
}

pub(super) struct TurnAliasState {
    pub(super) app_to_crp: HashMap<String, String>,
    pub(super) crp_to_app: HashMap<String, String>,
    pub(super) pending_compact_turns: VecDeque<String>,
    pub(super) active_app_turn_id: Option<String>,
    pub(super) active_crp_turn_id: Option<String>,
    pub(super) latest_token_usage: Option<ThreadTokenUsage>,
    pub(super) token_usage_by_app_turn: HashMap<String, ThreadTokenUsage>,
}

impl TurnAliasState {
    pub(super) fn new() -> Self {
        Self {
            app_to_crp: HashMap::new(),
            crp_to_app: HashMap::new(),
            pending_compact_turns: VecDeque::new(),
            active_app_turn_id: None,
            active_crp_turn_id: None,
            latest_token_usage: None,
            token_usage_by_app_turn: HashMap::new(),
        }
    }

    pub(super) fn bind_turn_alias(
        &mut self,
        app_turn_id: String,
        requested_crp_turn_id: Option<String>,
    ) -> String {
        let crp_turn_id = requested_crp_turn_id.unwrap_or_else(|| app_turn_id.clone());
        self.app_to_crp
            .insert(app_turn_id.clone(), crp_turn_id.clone());
        self.crp_to_app
            .insert(crp_turn_id.clone(), app_turn_id.clone());
        self.active_app_turn_id = Some(app_turn_id);
        self.active_crp_turn_id = Some(crp_turn_id.clone());
        crp_turn_id
    }

    pub(super) fn app_turn_id_for_crp(&self, crp_turn_id: Option<&str>) -> Option<String> {
        crp_turn_id.and_then(|id| self.crp_to_app.get(id).cloned())
    }

    pub(super) fn ensure_crp_turn_id(&mut self, app_turn_id: &str) -> String {
        if let Some(existing) = self.app_to_crp.get(app_turn_id) {
            return existing.clone();
        }
        if let Some(pending) = self.pending_compact_turns.pop_front() {
            return self.bind_turn_alias(app_turn_id.to_string(), Some(pending));
        }
        self.bind_turn_alias(app_turn_id.to_string(), None)
    }

    pub(super) fn note_terminal_turn(&mut self, app_turn_id: &str) {
        if self.active_app_turn_id.as_deref() == Some(app_turn_id) {
            self.active_app_turn_id = None;
        }
        if let Some(crp_turn_id) = self.app_to_crp.get(app_turn_id).cloned() {
            if self.active_crp_turn_id.as_deref() == Some(crp_turn_id.as_str()) {
                self.active_crp_turn_id = None;
            }
        }
    }

    pub(super) fn note_token_usage(&mut self, app_turn_id: &str, token_usage: ThreadTokenUsage) {
        self.latest_token_usage = Some(token_usage.clone());
        self.token_usage_by_app_turn
            .insert(app_turn_id.to_string(), token_usage);
    }

    pub(super) fn take_context_window_for_app_turn(&mut self, app_turn_id: &str) -> Option<Value> {
        let token_usage = self
            .token_usage_by_app_turn
            .remove(app_turn_id)
            .or_else(|| self.latest_token_usage.clone())?;
        canonical_context_window_from_thread_usage(&token_usage)
    }
}

pub(super) struct AppServerSessionState {
    pub(super) tracker: TurnTracker,
    pub(super) client: AppServerClient,
    pub(super) thread_id: String,
    pub(super) default_cwd: PathBuf,
    pub(super) default_model: String,
    pub(super) default_effort: Option<String>,
    pub(super) opened_commands: Vec<crate::protocol::CrpCommandInfo>,
    pub(super) opened_slash_commands: Vec<String>,
    pub(super) turn_aliases: TurnAliasState,
    pub(super) resumed_from_provider_session: bool,
    pub(super) command_execution_seen: bool,
}

pub(super) fn current_model_id(model: &str, effort: Option<&str>) -> String {
    match effort.map(str::trim).filter(|value| !value.is_empty()) {
        Some(effort) => format!("{model}/{effort}"),
        None => model.to_string(),
    }
}

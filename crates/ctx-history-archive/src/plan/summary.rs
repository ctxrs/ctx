use crate::{ArchiveIdentity, StableEntityId};
use ctx_history_index::{CoreEventRangeSelection, CoreEventRecord};
use serde::Serialize;

/// Counts describe selected local conversations before identical retained
/// original members from multiple import namespaces are coalesced on export.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScopeCounts {
    pub matched_records: u64,
    pub retained_records: u64,
    pub records_before_window: u64,
    pub records_after_window: u64,
    pub untimed_records: u64,
    pub records_without_workspace: u64,
    pub records_outside_workspace: u64,
    pub records_with_other_workspaces: u64,
}

impl ScopeCounts {
    pub(super) fn add(&mut self, other: &Self) {
        self.matched_records += other.matched_records;
        self.retained_records += other.retained_records;
        self.records_before_window += other.records_before_window;
        self.records_after_window += other.records_after_window;
        self.untimed_records += other.untimed_records;
        self.records_without_workspace += other.records_without_workspace;
        self.records_outside_workspace += other.records_outside_workspace;
        self.records_with_other_workspaces += other.records_with_other_workspaces;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportPreview {
    pub generation_id: String,
    pub identity: ArchiveIdentity,
    pub excluded_identities: Vec<ArchiveIdentity>,
    pub workspace: Option<String>,
    pub since_unix_ms: Option<i64>,
    pub until_unix_ms: Option<i64>,
    pub selection_complete: bool,
    pub selected_conversations: u64,
    pub scope: ScopeCounts,
    pub conversations: Vec<ConversationPreview>,
    pub conversations_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceValue {
    pub value: String,
    pub matches_workspace: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationPreview {
    pub ctx_session_id: String,
    pub provider: String,
    pub provider_session_id: Option<String>,
    pub first_occurred_at_unix_ms: Option<i64>,
    pub last_occurred_at_unix_ms: Option<i64>,
    pub scope: ScopeCounts,
    pub workspace_values: Vec<WorkspaceValue>,
    pub workspace_values_truncated: bool,
    pub show_session_command: String,
}

impl ConversationPreview {
    pub(super) fn new(session: StableEntityId, matched_records: u64) -> Self {
        let ctx_session_id = session.as_uuid().to_string();
        Self {
            show_session_command: format!("ctx show session {ctx_session_id} --mode log"),
            ctx_session_id,
            provider: String::new(),
            provider_session_id: None,
            first_occurred_at_unix_ms: None,
            last_occurred_at_unix_ms: None,
            scope: ScopeCounts {
                matched_records,
                ..ScopeCounts::default()
            },
            workspace_values: Vec::new(),
            workspace_values_truncated: false,
        }
    }

    pub(super) fn observe(&mut self, event: &CoreEventRecord, query: &CoreEventRangeSelection) {
        if self.scope.retained_records == 0 {
            self.provider.clone_from(&event.provider);
            self.provider_session_id
                .clone_from(&event.provider_session_id);
        }
        self.scope.retained_records += 1;
        if let Some(time) = event.core_record.occurred_at_unix_ms {
            self.first_occurred_at_unix_ms = Some(
                self.first_occurred_at_unix_ms
                    .map_or(time, |old| old.min(time)),
            );
            self.last_occurred_at_unix_ms = Some(
                self.last_occurred_at_unix_ms
                    .map_or(time, |old| old.max(time)),
            );
            self.scope.records_before_window +=
                u64::from(query.since_unix_ms().is_some_and(|since| time < since));
            self.scope.records_after_window +=
                u64::from(query.until_unix_ms().is_some_and(|until| time >= until));
        } else {
            self.scope.untimed_records += 1;
        }
        let (mut known, mut matched, mut other) = (false, false, false);
        for (value, matches_workspace) in query.workspace_metadata(&event.core_record) {
            known = true;
            matched |= matches_workspace;
            other |= !matches_workspace;
            let evidence = WorkspaceValue {
                value: value.to_owned(),
                matches_workspace,
            };
            if !self.workspace_values.contains(&evidence) {
                if self.workspace_values.len() < 8 {
                    self.workspace_values.push(evidence);
                } else {
                    self.workspace_values_truncated = true;
                }
            }
        }
        self.scope.records_without_workspace += u64::from(!known);
        self.scope.records_outside_workspace += u64::from(known && !matched);
        self.scope.records_with_other_workspaces += u64::from(other);
    }
}

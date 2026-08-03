use clap::{Args, ValueEnum};
use ctx_history_index::{
    CoreEventRangeDirection, CoreEventRangeDomain, CoreEventRangeScope, CoreEventRangeSelection,
};
use serde_json::{json, Map, Value};

use super::{format_timestamp, DEFAULT_EVENT_QUERY_LIMIT, EVENT_QUERY_PAGE_ITEMS};

#[derive(Debug, Args)]
pub(crate) struct ListEventsArgs {
    #[arg(
        long,
        requires = "until",
        help = "Inclusive millisecond-aligned absolute RFC3339 lower bound"
    )]
    pub(crate) since: Option<String>,
    #[arg(
        long,
        requires = "since",
        help = "Exclusive millisecond-aligned absolute RFC3339 upper bound"
    )]
    pub(crate) until: Option<String>,
    #[arg(
        long,
        help = "Filter by exact provider; repeat to select more than one"
    )]
    pub(crate) provider: Vec<String>,
    #[arg(long, help = "Filter by exact public ctx source UUID")]
    pub(crate) source: Option<String>,
    #[arg(
        long = "history-source",
        help = "Filter custom history source as provider-key/source-id"
    )]
    pub(crate) history_source: Option<String>,
    #[arg(long = "provider-key", help = "Filter by custom history provider key")]
    pub(crate) provider_key: Option<String>,
    #[arg(long = "source-id", help = "Filter by custom history source ID")]
    pub(crate) source_id: Option<String>,
    #[arg(long = "source-format", help = "Filter by exact indexed source format")]
    pub(crate) source_format: Option<String>,
    #[arg(
        long = "provider-session",
        help = "Filter by exact provider-native session ID"
    )]
    pub(crate) provider_session: Option<String>,
    #[arg(long, help = "Filter by exact public ctx session UUID")]
    pub(crate) session: Option<String>,
    #[arg(
        long = "parent-session",
        help = "Filter by exact public parent ctx session UUID"
    )]
    pub(crate) parent_session: Option<String>,
    #[arg(
        long = "root-session",
        help = "Filter by exact public root ctx session UUID"
    )]
    pub(crate) root_session: Option<String>,
    #[arg(long, help = "Filter by exact branch")]
    pub(crate) branch: Option<String>,
    #[arg(long, help = "Filter by case-insensitive workspace or cwd substring")]
    pub(crate) workspace: Option<String>,
    #[arg(
        long = "event-type",
        help = "Filter by exact event type, including provider-defined values"
    )]
    pub(crate) event_type: Option<String>,
    #[arg(long, help = "Filter by exact role")]
    pub(crate) role: Option<String>,
    #[arg(long = "agent-type", help = "Filter by exact agent type")]
    pub(crate) agent_type: Option<String>,
    #[arg(long, value_enum, default_value_t = EventQueryScope::All)]
    pub(crate) scope: EventQueryScope,
    #[arg(long, help = "Filter by case-insensitive touched-file substring")]
    pub(crate) file: Option<String>,
    #[arg(long, value_enum, default_value_t = EventQueryDirection::Ascending)]
    pub(crate) direction: EventQueryDirection,
    #[arg(long, help = "Resume from an opaque cursor returned by a prior page")]
    pub(crate) cursor: Option<String>,
    #[arg(long, default_value_t = DEFAULT_EVENT_QUERY_LIMIT, help = "Maximum events returned across the complete invocation")]
    pub(crate) limit: u64,
    #[arg(long, value_enum, default_value_t = EventContentProjection::Full)]
    pub(crate) content: EventContentProjection,
    #[arg(long, value_enum, default_value_t = EventQueryFormat::Json)]
    pub(crate) format: EventQueryFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventQueryFormat {
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventContentProjection {
    Full,
    Text,
    None,
}

impl EventContentProjection {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Text => "text",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventQueryScope {
    All,
    Primary,
    Subagent,
}

impl From<EventQueryScope> for CoreEventRangeScope {
    fn from(value: EventQueryScope) -> Self {
        match value {
            EventQueryScope::All => Self::All,
            EventQueryScope::Primary => Self::Primary,
            EventQueryScope::Subagent => Self::Subagent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventQueryDirection {
    Ascending,
    Descending,
}

impl From<EventQueryDirection> for CoreEventRangeDirection {
    fn from(value: EventQueryDirection) -> Self {
        match value {
            EventQueryDirection::Ascending => Self::Ascending,
            EventQueryDirection::Descending => Self::Descending,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EventQueryWireRequest {
    pub(crate) domain: Value,
    pub(crate) filters: Value,
    pub(crate) direction: &'static str,
    pub(crate) content: EventContentProjection,
    pub(crate) limit: usize,
}

impl EventQueryWireRequest {
    pub(crate) fn from_selection(
        selection: &CoreEventRangeSelection,
        content: EventContentProjection,
        limit: usize,
    ) -> Self {
        let selected = selection.filters();
        let mut filters = Map::new();
        if !selected.providers.is_empty() {
            filters.insert("providers".to_owned(), json!(selected.providers));
        }
        let source_identity = selected.source_identity.map(|value| value.to_string());
        let session_id = selected.session_id.map(|value| value.to_string());
        let parent_session_id = selected.parent_session_id.map(|value| value.to_string());
        let root_session_id = selected.root_session_id.map(|value| value.to_string());
        for (key, value) in [
            ("source", source_identity.as_deref()),
            ("history_source", selected.history_source.as_deref()),
            ("provider_key", selected.provider_key.as_deref()),
            ("source_id", selected.source_id.as_deref()),
            ("source_format", selected.source_format.as_deref()),
            (
                "provider_session_id",
                selected.provider_session_id.as_deref(),
            ),
            ("session", session_id.as_deref()),
            ("parent_session", parent_session_id.as_deref()),
            ("root_session", root_session_id.as_deref()),
            ("branch", selected.branch.as_deref()),
            ("workspace", selected.workspace.as_deref()),
            ("event_type", selected.event_type.as_deref()),
            ("role", selected.role.as_deref()),
            ("agent_type", selected.agent_type.as_deref()),
            ("file", selected.file.as_deref()),
        ] {
            insert_optional(&mut filters, key, value);
        }
        if selected.scope != CoreEventRangeScope::All {
            filters.insert(
                "scope".to_owned(),
                json!(match selected.scope {
                    CoreEventRangeScope::All => "all",
                    CoreEventRangeScope::Primary => "primary",
                    CoreEventRangeScope::Subagent => "subagent",
                }),
            );
        }
        let domain = match selection.domain() {
            CoreEventRangeDomain::All => json!({ "kind": "all" }),
            CoreEventRangeDomain::Timestamped {
                since_unix_ms,
                until_unix_ms,
            } => json!({
                "kind": "range",
                "range": {
                    "since": format_timestamp(Some(since_unix_ms)),
                    "until": format_timestamp(Some(until_unix_ms)),
                },
            }),
        };
        Self {
            domain,
            filters: Value::Object(filters),
            direction: match selected.direction {
                CoreEventRangeDirection::Ascending => "ascending",
                CoreEventRangeDirection::Descending => "descending",
            },
            content,
            limit,
        }
    }

    #[cfg(test)]
    pub(crate) fn new(
        domain: Value,
        filters: Value,
        direction: CoreEventRangeDirection,
        content: EventContentProjection,
        limit: usize,
    ) -> Self {
        Self {
            domain,
            filters,
            direction: match direction {
                CoreEventRangeDirection::Ascending => "ascending",
                CoreEventRangeDirection::Descending => "descending",
            },
            content,
            limit,
        }
    }

    pub(super) fn page_items(&self) -> usize {
        self.limit.min(EVENT_QUERY_PAGE_ITEMS)
    }
}

fn insert_optional(object: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), json!(value));
    }
}

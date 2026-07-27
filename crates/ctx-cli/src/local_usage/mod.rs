use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, BlameTarget, CommitPredicate, FactState, ProductionRelationship,
    PullRequestBlameRelationship,
};
use serde_json::Value;

use crate::cli::{CommandRoot, DaemonCommand};

mod report;
mod store;

#[cfg(test)]
mod tests;

pub(crate) use report::{pro_conversion_action, read_report, render_human_summary, UsageReport};
pub(crate) use store::{reset, UsageStoreError};

pub(crate) const DEFINITION_VERSION: i64 = 1;
pub(crate) const RETENTION_DAYS: i64 = 400;
pub(crate) const CTX_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Cli,
    Mcp,
}

impl Surface {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    Failure,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueClass {
    ResultBearing,
    Empty,
    NotApplicable,
}

impl ValueClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ResultBearing => "result_bearing",
            Self::Empty => "empty",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetType {
    File,
    Commit,
    PullRequest,
    NotApplicable,
}

impl TargetType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Commit => "commit",
            Self::PullRequest => "pull_request",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProOutcome {
    Produced,
    Possible,
    None,
    Error,
    NotApplicable,
}

impl ProOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Produced => "produced",
            Self::Possible => "possible",
            Self::None => "none",
            Self::Error => "error",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationBucket {
    Under10Ms,
    Ms10To49,
    Ms50To249,
    Ms250To999,
    Sec1To4,
    Sec5To29,
    Sec30Plus,
}

impl DurationBucket {
    fn from_duration(duration: Duration) -> Self {
        match duration.as_millis() {
            0..=9 => Self::Under10Ms,
            10..=49 => Self::Ms10To49,
            50..=249 => Self::Ms50To249,
            250..=999 => Self::Ms250To999,
            1_000..=4_999 => Self::Sec1To4,
            5_000..=29_999 => Self::Sec5To29,
            _ => Self::Sec30Plus,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Under10Ms => "under_10_ms",
            Self::Ms10To49 => "10_to_49_ms",
            Self::Ms50To249 => "50_to_249_ms",
            Self::Ms250To999 => "250_to_999_ms",
            Self::Sec1To4 => "1_to_4_s",
            Self::Sec5To29 => "5_to_29_s",
            Self::Sec30Plus => "30_s_or_more",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompletedOperation {
    surface: Surface,
    operation: &'static str,
    outcome: Outcome,
    value_class: ValueClass,
    duration: DurationBucket,
    target_type: TargetType,
    pro_outcome: ProOutcome,
    result_count: u64,
    citation_count: u64,
    response_bytes: u64,
}

impl CompletedOperation {
    pub(crate) fn cli(operation: &'static str, success: bool, duration: Duration) -> Self {
        Self {
            surface: Surface::Cli,
            operation,
            outcome: if success {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            value_class: ValueClass::NotApplicable,
            duration: DurationBucket::from_duration(duration),
            target_type: TargetType::NotApplicable,
            pro_outcome: ProOutcome::NotApplicable,
            result_count: 0,
            citation_count: 0,
            response_bytes: 0,
        }
    }

    pub(crate) fn with_value(mut self, value_class: ValueClass) -> Self {
        self.value_class = value_class;
        self
    }

    #[cfg(test)]
    pub(crate) const fn result_metadata_for_test(self) -> (ValueClass, u64, u64) {
        (self.value_class, self.result_count, self.citation_count)
    }

    #[cfg(test)]
    pub(crate) const fn target_type_for_test(self) -> TargetType {
        self.target_type
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CliUsage {
    operation: Option<&'static str>,
    target_type: TargetType,
    pro_outcome: ProOutcome,
    result_count: usize,
    citation_count: usize,
    value_class: ValueClass,
}

impl CliUsage {
    pub(crate) fn from_command(command: &CommandRoot) -> Self {
        let (operation, target_type) = match command {
            CommandRoot::Setup(_) => (Some("setup"), TargetType::NotApplicable),
            CommandRoot::Status(_) => (None, TargetType::NotApplicable),
            CommandRoot::Index(_) => (Some("index"), TargetType::NotApplicable),
            CommandRoot::Sources(_) => (Some("sources"), TargetType::NotApplicable),
            CommandRoot::Import(_) => (Some("import"), TargetType::NotApplicable),
            CommandRoot::Show(_) => (Some("show"), TargetType::NotApplicable),
            CommandRoot::Locate(_) => (Some("locate"), TargetType::NotApplicable),
            CommandRoot::Search(_) => (Some("search"), TargetType::NotApplicable),
            CommandRoot::Pro(args) => (
                Some(args.local_usage_operation()),
                TargetType::NotApplicable,
            ),
            CommandRoot::Blame(_) => (Some("blame"), TargetType::NotApplicable),
            CommandRoot::Sql(_) => (Some("sql"), TargetType::NotApplicable),
            CommandRoot::Docs(_) => (Some("docs"), TargetType::NotApplicable),
            CommandRoot::Integrations(_) => (Some("integrations"), TargetType::NotApplicable),
            CommandRoot::Mcp(_) => (None, TargetType::NotApplicable),
            CommandRoot::Daemon(args) => match &args.command {
                DaemonCommand::Status(_) => (Some("daemon_status"), TargetType::NotApplicable),
                DaemonCommand::Enable(_) => (Some("daemon_enable"), TargetType::NotApplicable),
                DaemonCommand::Disable(_) => (Some("daemon_disable"), TargetType::NotApplicable),
                DaemonCommand::Run(_) => (None, TargetType::NotApplicable),
            },
            CommandRoot::Upgrade(args) if !args.replacement_helper => {
                (Some("upgrade"), TargetType::NotApplicable)
            }
            CommandRoot::Upgrade(_) => (None, TargetType::NotApplicable),
            CommandRoot::Doctor(_) => (Some("doctor"), TargetType::NotApplicable),
        };
        Self {
            operation,
            target_type,
            pro_outcome: ProOutcome::NotApplicable,
            result_count: 0,
            citation_count: 0,
            value_class: ValueClass::NotApplicable,
        }
    }

    pub(crate) fn set_blame_result(&mut self, result: &BlameResult) {
        self.result_count = result.matches.len();
        self.citation_count = result.evidence.len();
        self.value_class = if result.matches.is_empty() {
            ValueClass::Empty
        } else {
            ValueClass::ResultBearing
        };
        self.pro_outcome = classify_blame(result);
    }

    pub(crate) fn bind_blame_target(&mut self, target: &BlameTarget) {
        self.target_type = match target {
            BlameTarget::File { .. } => TargetType::File,
            BlameTarget::Commit { .. } => TargetType::Commit,
            BlameTarget::PullRequest { .. } => TargetType::PullRequest,
        };
    }

    pub(crate) fn completed(self, success: bool, duration: Duration) -> Option<CompletedOperation> {
        let operation = self.operation?;
        let mut completed =
            CompletedOperation::cli(operation, success, duration).with_value(if success {
                self.value_class
            } else {
                ValueClass::NotApplicable
            });
        completed.target_type = self.target_type;
        if operation == "blame" {
            completed.pro_outcome = if success {
                self.pro_outcome
            } else {
                ProOutcome::Error
            };
            if success {
                completed.result_count = u64::try_from(self.result_count).unwrap_or(u64::MAX);
                completed.citation_count = u64::try_from(self.citation_count).unwrap_or(u64::MAX);
            }
        }
        Some(completed)
    }
}

pub(crate) fn record_best_effort(data_root: &Path, enabled: bool, operation: CompletedOperation) {
    if enabled {
        let _ = store::record(data_root, operation);
    }
}

#[cfg(test)]
fn record_best_effort_with_ctx_version_for_test(
    data_root: &Path,
    operation: CompletedOperation,
    ctx_version: &str,
) {
    let _ = store::record_with_ctx_version_for_test(data_root, operation, ctx_version);
}

pub(crate) struct McpUsageRecorder {
    data_root: PathBuf,
    enabled: bool,
    control_resolver: crate::config::LocalUsageConfigResolver,
    #[cfg(test)]
    trace: Option<std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>>,
}

impl McpUsageRecorder {
    pub(crate) fn start(data_root: PathBuf) -> Self {
        let mut control_resolver = crate::config::LocalUsageConfigResolver::default();
        let enabled = control_resolver.resolve(&data_root).effective_on_startup();
        Self {
            data_root,
            enabled,
            control_resolver,
            #[cfg(test)]
            trace: None,
        }
    }

    pub(crate) fn record_delivered(
        &mut self,
        invocation: McpInvocation,
        response: &Value,
        duration: Duration,
        serialized_response_bytes: usize,
    ) {
        self.refresh_control();
        if self.enabled {
            record_best_effort(
                &self.data_root,
                true,
                invocation.completed(response, duration, serialized_response_bytes),
            );
            #[cfg(test)]
            if let Some(trace) = &self.trace {
                trace.lock().unwrap().push("local_usage");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_test_trace(
        &mut self,
        trace: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    ) {
        self.trace = Some(trace);
    }

    fn refresh_control(&mut self) {
        self.enabled = self
            .control_resolver
            .resolve(&self.data_root)
            .effective_after(Some(self.enabled));
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct McpInvocation {
    operation: &'static str,
    target_type: TargetType,
}

impl McpInvocation {
    pub(crate) fn recognized(name: &str) -> Option<Self> {
        let operation = match name {
            "status" => "status",
            "sources" => "sources",
            "search" => "search",
            "sql" => "sql",
            "show_session" => "show_session",
            "show_event" => "show_event",
            "pro_status" => "pro_status",
            "blame" => "blame",
            _ => return None,
        };
        Some(Self {
            operation,
            target_type: TargetType::NotApplicable,
        })
    }

    pub(crate) fn bind_blame_target(&mut self, target: &BlameTarget) {
        self.target_type = match target {
            BlameTarget::File { .. } => TargetType::File,
            BlameTarget::Commit { .. } => TargetType::Commit,
            BlameTarget::PullRequest { .. } => TargetType::PullRequest,
        };
    }

    #[cfg(test)]
    pub(crate) const fn target_type_for_test(self) -> TargetType {
        self.target_type
    }

    pub(crate) fn completed(
        self,
        response: &Value,
        duration: Duration,
        response_bytes: usize,
    ) -> CompletedOperation {
        let failed = response.get("error").is_some()
            || response.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
        let mut operation = CompletedOperation {
            surface: Surface::Mcp,
            operation: self.operation,
            outcome: if failed {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            value_class: ValueClass::NotApplicable,
            duration: DurationBucket::from_duration(duration),
            target_type: self.target_type,
            pro_outcome: if self.operation == "blame" && failed {
                ProOutcome::Error
            } else {
                ProOutcome::NotApplicable
            },
            result_count: 0,
            citation_count: 0,
            response_bytes: u64::try_from(response_bytes).unwrap_or(u64::MAX),
        };
        if !failed {
            let structured = response.pointer("/result/structuredContent");
            let count = mcp_result_count(self.operation, structured);
            if let Some(count) = count {
                operation.value_class = if count == 0 {
                    ValueClass::Empty
                } else {
                    ValueClass::ResultBearing
                };
                operation.result_count = u64::try_from(count).unwrap_or(u64::MAX);
            }
            if self.operation == "blame" {
                operation.citation_count = structured
                    .and_then(|value| value.get("evidence"))
                    .and_then(Value::as_array)
                    .map_or(0, |values| u64::try_from(values.len()).unwrap_or(u64::MAX));
                operation.pro_outcome = classify_blame_json(structured);
            }
        }
        operation
    }
}

fn mcp_result_count(operation: &str, structured: Option<&Value>) -> Option<usize> {
    let structured = structured?;
    let field = match operation {
        "sources" => "sources",
        "search" => "results",
        "sql" => "rows",
        "show_session" | "show_event" => "events",
        "blame" => "matches",
        _ => return None,
    };
    structured
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
}

pub(crate) fn classify_blame(result: &BlameResult) -> ProOutcome {
    let mut possible = false;
    for item in &result.matches {
        match item {
            BlameMatch::File(value) => {
                for production in &value.production {
                    match classify_production(production.relationship, production.state) {
                        ProOutcome::Produced => return ProOutcome::Produced,
                        ProOutcome::Possible => possible = true,
                        ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                    }
                }
            }
            BlameMatch::Commit(value) => {
                match classify_commit_predicate(value.predicate, value.state) {
                    ProOutcome::Produced => return ProOutcome::Produced,
                    ProOutcome::Possible => possible = true,
                    ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                }
            }
            BlameMatch::PullRequest(value) => match &value.relationship {
                PullRequestBlameRelationship::Commit(commit) => {
                    // Commit membership is a reference-only result even when no
                    // asserted production attribution is present.
                    possible = true;
                    for production in &commit.production {
                        match classify_production(production.relationship, production.state) {
                            ProOutcome::Produced => return ProOutcome::Produced,
                            ProOutcome::Possible => possible = true,
                            ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                        }
                    }
                }
                PullRequestBlameRelationship::Activity(activity)
                    if matches!(activity.state, FactState::Asserted | FactState::Ambiguous) =>
                {
                    possible = true;
                }
                PullRequestBlameRelationship::Activity(_) => {}
            },
        }
    }
    if possible {
        ProOutcome::Possible
    } else {
        ProOutcome::None
    }
}

fn classify_blame_json(structured: Option<&Value>) -> ProOutcome {
    let Some(matches) = structured
        .and_then(|value| value.get("matches"))
        .and_then(Value::as_array)
    else {
        return ProOutcome::None;
    };
    let mut possible = false;
    for item in matches {
        match item.get("kind").and_then(Value::as_str) {
            Some("file") => {
                if production_outcome(item.pointer("/value/production"), &mut possible) {
                    return ProOutcome::Produced;
                }
            }
            Some("commit") => {
                let predicate = item
                    .pointer("/value/predicate")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<CommitPredicate>(value).ok());
                let state = item
                    .pointer("/value/state")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<FactState>(value).ok());
                if let (Some(predicate), Some(state)) = (predicate, state) {
                    match classify_commit_predicate(predicate, state) {
                        ProOutcome::Produced => return ProOutcome::Produced,
                        ProOutcome::Possible => possible = true,
                        ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                    }
                }
            }
            Some("pull_request") => {
                let relationship = item.pointer("/value/relationship");
                match relationship
                    .and_then(|value| value.get("kind"))
                    .and_then(Value::as_str)
                {
                    Some("commit") => {
                        possible = true;
                        if production_outcome(
                            relationship.and_then(|value| value.pointer("/value/production")),
                            &mut possible,
                        ) {
                            return ProOutcome::Produced;
                        }
                    }
                    Some("activity")
                        if relationship
                            .and_then(|value| value.pointer("/value/state"))
                            .and_then(Value::as_str)
                            .is_some_and(|state| matches!(state, "asserted" | "ambiguous")) =>
                    {
                        possible = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if possible {
        ProOutcome::Possible
    } else {
        ProOutcome::None
    }
}

fn classify_commit_predicate(predicate: CommitPredicate, state: FactState) -> ProOutcome {
    match state {
        FactState::Asserted if predicate == CommitPredicate::ProducedBy => ProOutcome::Produced,
        FactState::Asserted | FactState::Ambiguous => ProOutcome::Possible,
        FactState::Contradicted | FactState::Superseded => ProOutcome::None,
    }
}

fn production_outcome(production: Option<&Value>, possible: &mut bool) -> bool {
    let Some(production) = production.and_then(Value::as_array) else {
        return false;
    };
    for attribution in production {
        let relationship = attribution
            .get("relationship")
            .cloned()
            .and_then(|value| serde_json::from_value::<ProductionRelationship>(value).ok());
        let state = attribution
            .get("state")
            .cloned()
            .and_then(|value| serde_json::from_value::<FactState>(value).ok());
        if let (Some(relationship), Some(state)) = (relationship, state) {
            match classify_production(relationship, state) {
                ProOutcome::Produced => return true,
                ProOutcome::Possible => *possible = true,
                ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
            }
        }
    }
    false
}

fn classify_production(relationship: ProductionRelationship, state: FactState) -> ProOutcome {
    match (relationship, state) {
        (ProductionRelationship::ProducedBy, FactState::Asserted) => ProOutcome::Produced,
        (
            ProductionRelationship::ProducedBy | ProductionRelationship::PossiblyProducedBy,
            FactState::Asserted | FactState::Ambiguous,
        ) => ProOutcome::Possible,
        (
            ProductionRelationship::ProducedBy | ProductionRelationship::PossiblyProducedBy,
            FactState::Contradicted | FactState::Superseded,
        ) => ProOutcome::None,
    }
}

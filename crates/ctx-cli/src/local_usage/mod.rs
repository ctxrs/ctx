use std::{collections::BTreeSet, path::Path, time::Duration};

use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, BlameTarget, CommitPredicate, FactState, ProductionRelationship,
    PullRequestBlameRelationship,
};
use serde_json::Value;

use crate::cli::{CommandRoot, DaemonCommand, ShowTarget};

mod correlation;
mod estimate;
mod report;
mod store;

#[cfg(test)]
mod tests;

pub(crate) use correlation::McpUsageRecorder;
pub(crate) use estimate::{estimate_usage, EstimateFacts, UsageEstimates};
pub(crate) use report::{pro_conversion_action, read_report, UsageReport};
pub(crate) use store::{reset, UsageStoreError};

pub(crate) const DEFINITION_VERSION: i64 = 2;
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

/// Closed adapter vocabulary retained at the public result boundary.
///
/// Definition 2 persists the public operation itself, not this adapter hint.
/// The hint lets existing command adapters classify their canonical result
/// collections without accepting content or an open-ended operation string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultObservationAction {
    Search,
    OpenSession,
    OpenEvent,
    Locate,
    Sources,
    Blame,
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
pub(crate) enum ContextCoverage {
    Complete,
    Unavailable,
    NotApplicable,
}

impl ContextCoverage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Unavailable => "unavailable",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SearchContextObservation {
    coverage: ContextCoverage,
    delivered_context_bytes: u64,
    matched_normalized_session_bytes: u64,
}

impl SearchContextObservation {
    pub(crate) const fn unavailable() -> Self {
        Self {
            coverage: ContextCoverage::Unavailable,
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        }
    }

    pub(crate) fn complete(
        delivered_context_bytes: usize,
        matched_normalized_session_bytes: usize,
    ) -> Option<Self> {
        let delivered_context_bytes = u64::try_from(delivered_context_bytes).ok()?;
        let matched_normalized_session_bytes =
            u64::try_from(matched_normalized_session_bytes).ok()?;
        (delivered_context_bytes > 0
            && matched_normalized_session_bytes > 0
            && matched_normalized_session_bytes >= delivered_context_bytes)
            .then_some(Self {
                coverage: ContextCoverage::Complete,
                delivered_context_bytes,
                matched_normalized_session_bytes,
            })
    }

    #[cfg(test)]
    pub(crate) const fn metadata_for_test(self) -> (ContextCoverage, u64, u64) {
        (
            self.coverage,
            self.delivered_context_bytes,
            self.matched_normalized_session_bytes,
        )
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
    context_coverage: ContextCoverage,
    result_count: u64,
    citation_count: u64,
    delivered_output_bytes: u64,
    delivered_context_bytes: u64,
    matched_normalized_session_bytes: u64,
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
            context_coverage: ContextCoverage::NotApplicable,
            result_count: 0,
            citation_count: 0,
            delivered_output_bytes: 0,
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_value(mut self, value_class: ValueClass) -> Self {
        self.value_class = value_class;
        self
    }

    #[cfg(test)]
    pub(crate) const fn result_metadata_for_test(self) -> (ValueClass, u64, u64) {
        (self.value_class, self.result_count, self.citation_count)
    }

    #[cfg(test)]
    pub(crate) const fn context_metadata_for_test(self) -> (ContextCoverage, u64, u64) {
        (
            self.context_coverage,
            self.delivered_context_bytes,
            self.matched_normalized_session_bytes,
        )
    }

    #[cfg(test)]
    pub(crate) const fn delivered_output_bytes_for_test(self) -> u64 {
        self.delivered_output_bytes
    }

    #[cfg(test)]
    pub(crate) const fn duration_bucket_for_test(self) -> &'static str {
        self.duration.as_str()
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
    output_bytes: usize,
    output_bytes_measured: bool,
    result_action: Option<ResultObservationAction>,
    search_context: Option<SearchContextObservation>,
    value_class: ValueClass,
}

impl CliUsage {
    pub(crate) fn from_command(command: &CommandRoot) -> Self {
        let (operation, target_type) = match command {
            CommandRoot::Setup(_) => (Some("setup"), TargetType::NotApplicable),
            CommandRoot::Status(_) | CommandRoot::Stats(_) => (None, TargetType::NotApplicable),
            CommandRoot::Index(_) => (Some("index"), TargetType::NotApplicable),
            CommandRoot::Sources(_) => (Some("sources"), TargetType::NotApplicable),
            CommandRoot::Import(_) => (Some("import"), TargetType::NotApplicable),
            CommandRoot::Show(args) => (
                match &args.target {
                    ShowTarget::Session(_) => Some("show_session"),
                    ShowTarget::Event(_) => Some("show_event"),
                },
                TargetType::NotApplicable,
            ),
            CommandRoot::List(_) => (Some("show_event"), TargetType::NotApplicable),
            CommandRoot::Locate(_) => (Some("locate"), TargetType::NotApplicable),
            CommandRoot::Search(_) => (Some("search"), TargetType::NotApplicable),
            CommandRoot::Pro(args) => (
                Some(args.local_usage_operation()),
                TargetType::NotApplicable,
            ),
            CommandRoot::Referral(_) => (None, TargetType::NotApplicable),
            CommandRoot::Blame(_) => (Some("blame"), TargetType::NotApplicable),
            CommandRoot::Docs(_) => (Some("docs"), TargetType::NotApplicable),
            CommandRoot::Integrations(_) => (Some("integrations"), TargetType::NotApplicable),
            CommandRoot::Mcp(_) => (None, TargetType::NotApplicable),
            CommandRoot::Daemon(args) => match &args.command {
                DaemonCommand::Status(_) => (Some("daemon_status"), TargetType::NotApplicable),
                DaemonCommand::Enable(_) => (Some("daemon_enable"), TargetType::NotApplicable),
                DaemonCommand::Disable(_) => (Some("daemon_disable"), TargetType::NotApplicable),
                DaemonCommand::Run(_) => (None, TargetType::NotApplicable),
            },
            CommandRoot::Upgrade(args)
                if !args.replacement_helper && args.hosted_transaction.is_none() =>
            {
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
            output_bytes: 0,
            output_bytes_measured: false,
            result_action: None,
            search_context: None,
            value_class: ValueClass::NotApplicable,
        }
    }

    pub(crate) fn excluded() -> Self {
        Self {
            operation: None,
            target_type: TargetType::NotApplicable,
            pro_outcome: ProOutcome::NotApplicable,
            result_count: 0,
            citation_count: 0,
            output_bytes: 0,
            output_bytes_measured: false,
            result_action: None,
            search_context: None,
            value_class: ValueClass::NotApplicable,
        }
    }

    /// Accepts only bounded numeric observations from the canonical result.
    ///
    /// `content_bytes` is intentionally ignored by definition 2. Historical
    /// adapters supplied JSON framing or non-search payload bytes here; only
    /// the dedicated complete-search observation can populate context facts.
    pub(crate) fn set_result_observation(
        &mut self,
        action: ResultObservationAction,
        result_count: usize,
        citation_count: usize,
        _content_bytes: usize,
    ) {
        self.result_count = result_count;
        self.citation_count = if action == ResultObservationAction::Blame {
            citation_count
        } else {
            0
        };
        self.result_action = Some(action);
        self.value_class = if result_count == 0 {
            ValueClass::Empty
        } else {
            ValueClass::ResultBearing
        };
    }

    pub(crate) fn set_measured_output_bytes(&mut self, output_bytes: usize) {
        self.output_bytes = output_bytes;
        self.output_bytes_measured = true;
    }

    pub(crate) fn set_search_context_observation(&mut self, observation: SearchContextObservation) {
        self.search_context = Some(observation);
    }

    pub(crate) fn set_blame_result(&mut self, result: &BlameResult) {
        self.set_result_observation(
            ResultObservationAction::Blame,
            result.matches.len(),
            unique_blame_citation_count(result),
            0,
        );
        self.pro_outcome = classify_blame(result);
    }

    pub(crate) fn bind_blame_target(&mut self, target: &BlameTarget) {
        self.target_type = target_type(target);
    }

    pub(crate) fn completed(self, success: bool, duration: Duration) -> Option<CompletedOperation> {
        let operation = self.operation?;
        let mut completed = CompletedOperation::cli(operation, success, duration);
        completed.target_type = self.target_type;
        completed.delivered_output_bytes = if self.output_bytes_measured {
            u64::try_from(self.output_bytes).unwrap_or(u64::MAX)
        } else {
            0
        };
        if !success {
            if operation == "blame" {
                completed.pro_outcome = ProOutcome::Error;
            }
            return Some(completed);
        }
        if matches!(operation, "search" | "blame") {
            completed.value_class = self.value_class;
            completed.result_count = u64::try_from(self.result_count).unwrap_or(u64::MAX);
        }
        if operation == "blame" {
            completed.pro_outcome = self.pro_outcome;
            completed.citation_count = u64::try_from(self.citation_count).unwrap_or(u64::MAX);
        }
        if operation == "search" && self.value_class == ValueClass::ResultBearing {
            let observation = self
                .search_context
                .unwrap_or_else(SearchContextObservation::unavailable);
            completed.context_coverage = observation.coverage;
            completed.delivered_context_bytes = observation.delivered_context_bytes;
            completed.matched_normalized_session_bytes =
                observation.matched_normalized_session_bytes;
        }
        Some(completed)
    }
}

pub(crate) fn record_best_effort(data_root: &Path, enabled: bool, operation: CompletedOperation) {
    if enabled {
        let _ = store::record(data_root, operation);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum McpContextTarget {
    Session(String),
    Event(String),
}

#[derive(Debug, Clone)]
pub(crate) struct McpInvocation {
    operation: &'static str,
    target_type: TargetType,
    search_context: Option<SearchContextObservation>,
}

impl McpInvocation {
    pub(crate) fn recognized(name: &str) -> Option<Self> {
        let operation = match name {
            "status" => "status",
            "sources" => "sources",
            "search" => "search",
            "show_session" => "show_session",
            "show_event" => "show_event",
            "query_events" => "show_event",
            "pro_status" => "pro_status",
            "blame" => "blame",
            _ => return None,
        };
        Some(Self {
            operation,
            target_type: TargetType::NotApplicable,
            search_context: None,
        })
    }

    pub(crate) fn bind_search_context(&mut self, observation: SearchContextObservation) {
        if self.operation == "search" {
            self.search_context = Some(observation);
        }
    }

    pub(crate) fn bind_blame_target(&mut self, target: &BlameTarget) {
        self.target_type = target_type(target);
    }

    #[cfg(test)]
    pub(crate) fn target_type_for_test(self) -> TargetType {
        self.target_type
    }

    pub(crate) fn completed(
        &self,
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
            context_coverage: ContextCoverage::NotApplicable,
            result_count: 0,
            citation_count: 0,
            delivered_output_bytes: u64::try_from(response_bytes).unwrap_or(u64::MAX),
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        };
        if failed {
            return operation;
        }
        let structured = response.pointer("/result/structuredContent");
        if let Some(count) = mcp_result_count(self.operation, structured) {
            operation.value_class = if count == 0 {
                ValueClass::Empty
            } else {
                ValueClass::ResultBearing
            };
            operation.result_count = u64::try_from(count).unwrap_or(u64::MAX);
        }
        if self.operation == "search" && operation.value_class == ValueClass::ResultBearing {
            let observation = self
                .search_context
                .unwrap_or_else(SearchContextObservation::unavailable);
            operation.context_coverage = observation.coverage;
            operation.delivered_context_bytes = observation.delivered_context_bytes;
            operation.matched_normalized_session_bytes =
                observation.matched_normalized_session_bytes;
        }
        if self.operation == "blame" {
            operation.citation_count = unique_blame_json_citation_count(structured);
            operation.pro_outcome = classify_blame_json(structured);
        }
        operation
    }
}

fn target_type(target: &BlameTarget) -> TargetType {
    match target {
        BlameTarget::File { .. } => TargetType::File,
        BlameTarget::Commit { .. } => TargetType::Commit,
        BlameTarget::PullRequest { .. } => TargetType::PullRequest,
    }
}

fn mcp_search_context_targets(response: &Value) -> Vec<McpContextTarget> {
    response
        .pointer("/result/structuredContent/results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|result| {
                    match (
                        result.get("result_scope").and_then(Value::as_str),
                        result.get("result_type").and_then(Value::as_str),
                    ) {
                        (Some("session"), Some("session_result")) => result
                            .get("ctx_session_id")
                            .and_then(Value::as_str)
                            .map(|id| McpContextTarget::Session(id.to_owned())),
                        (Some("event"), Some("event")) => result
                            .get("ctx_event_id")
                            .and_then(Value::as_str)
                            .map(|id| McpContextTarget::Event(id.to_owned())),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn resolved_mcp_context_target(operation: &str, response: &Value) -> Option<McpContextTarget> {
    let structured = response.pointer("/result/structuredContent")?;
    match operation {
        "show_session" => structured
            .get("ctx_session_id")
            .and_then(Value::as_str)
            .map(|id| McpContextTarget::Session(id.to_owned())),
        "show_event" => structured
            .get("ctx_event_id")
            .and_then(Value::as_str)
            .map(|id| McpContextTarget::Event(id.to_owned())),
        _ => None,
    }
}

fn mcp_result_count(operation: &str, structured: Option<&Value>) -> Option<usize> {
    let structured = structured?;
    let field = match operation {
        "sources" => "sources",
        "search" => "results",
        "show_session" | "show_event" => "events",
        "blame" => "matches",
        _ => return None,
    };
    structured
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
}

fn unique_blame_citation_count(result: &BlameResult) -> usize {
    result
        .evidence
        .iter()
        .map(|evidence| evidence.number)
        .collect::<BTreeSet<_>>()
        .len()
}

fn unique_blame_json_citation_count(structured: Option<&Value>) -> u64 {
    structured
        .and_then(|value| value.get("evidence"))
        .and_then(Value::as_array)
        .map(|evidence| {
            evidence
                .iter()
                .filter_map(|value| value.get("number").and_then(Value::as_u64))
                .collect::<BTreeSet<_>>()
        })
        .and_then(|numbers| u64::try_from(numbers.len()).ok())
        .unwrap_or(0)
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

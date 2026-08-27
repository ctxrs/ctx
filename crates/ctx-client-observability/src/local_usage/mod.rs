use crate::operation_descriptor::{
    LocalUsageOperation, ObservedMcpProductOperation, OperationDescriptor,
};
use std::time::Duration;

pub use crate::operation_descriptor::ResultObservationAction;

mod authority;
mod correlation;
mod estimate;
mod report;
mod store;

#[cfg(test)]
mod tests;

pub use authority::{LocalUsageStorageAuthority, UsageControlRevision, UsageControlSnapshot};
pub use correlation::McpUsageRecorder;
pub use estimate::{estimate_usage, EstimateFacts, UsageEstimates};
#[cfg(test)]
pub use report::read_report;
pub use report::{read_report_authorized, UsageReport};
#[cfg(test)]
pub use store::reset;
pub use store::{reset_authorized, UsageStoreError};

pub const DEFINITION_VERSION: i64 = 3;
pub const RETENTION_DAYS: i64 = 400;
pub const USAGE_REPORT_SCHEMA_VERSION: i64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
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
pub enum Outcome {
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
pub enum ValueClass {
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
pub enum ContextCoverage {
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
pub struct SearchContextObservation {
    coverage: ContextCoverage,
    delivered_context_bytes: u64,
    matched_normalized_session_bytes: u64,
}

impl SearchContextObservation {
    pub const fn unavailable() -> Self {
        Self {
            coverage: ContextCoverage::Unavailable,
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        }
    }

    pub fn complete(
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

    pub const fn complete_byte_totals(self) -> Option<(u64, u64)> {
        match self.coverage {
            ContextCoverage::Complete => Some((
                self.delivered_context_bytes,
                self.matched_normalized_session_bytes,
            )),
            ContextCoverage::Unavailable | ContextCoverage::NotApplicable => None,
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
pub struct CompletedOperation {
    definition_version: i64,
    surface: Surface,
    operation: LocalUsageOperation,
    outcome: Outcome,
    value_class: ValueClass,
    duration: DurationBucket,
    context_coverage: ContextCoverage,
    result_count: u64,
    delivered_output_bytes: u64,
    delivered_context_bytes: u64,
    matched_normalized_session_bytes: u64,
}

impl CompletedOperation {
    pub fn cli(operation: LocalUsageOperation, success: bool, duration: Duration) -> Self {
        Self {
            definition_version: DEFINITION_VERSION,
            surface: Surface::Cli,
            operation,
            outcome: if success {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            value_class: ValueClass::NotApplicable,
            duration: DurationBucket::from_duration(duration),
            context_coverage: ContextCoverage::NotApplicable,
            result_count: 0,
            delivered_output_bytes: 0,
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        }
    }

    /// Builds the aggregate-only completion observed by Core's existing
    /// companion wrapper. Core does not inspect private result semantics, so
    /// Blame result classification and count remain not applicable.
    pub fn blame(
        surface: Surface,
        success: bool,
        delivered_output_bytes: usize,
        duration: Duration,
    ) -> Self {
        Self {
            definition_version: DEFINITION_VERSION,
            surface,
            operation: LocalUsageOperation::Blame,
            outcome: if success {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            value_class: ValueClass::NotApplicable,
            duration: DurationBucket::from_duration(duration),
            context_coverage: ContextCoverage::NotApplicable,
            result_count: 0,
            delivered_output_bytes: u64::try_from(delivered_output_bytes).unwrap_or(u64::MAX),
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_value(mut self, value_class: ValueClass) -> Self {
        self.value_class = value_class;
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn result_metadata_for_test(self) -> (ValueClass, u64) {
        (self.value_class, self.result_count)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn definition_version_for_test(self) -> i64 {
        self.definition_version
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn context_metadata_for_test(self) -> (ContextCoverage, u64, u64) {
        (
            self.context_coverage,
            self.delivered_context_bytes,
            self.matched_normalized_session_bytes,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn delivered_output_bytes_for_test(self) -> u64 {
        self.delivered_output_bytes
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn duration_bucket_for_test(self) -> &'static str {
        self.duration.as_str()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CliUsage {
    operation: Option<LocalUsageOperation>,
    result_count: usize,
    output_bytes: usize,
    search_context: Option<SearchContextObservation>,
    value_class: ValueClass,
}

impl CliUsage {
    pub fn from_descriptor(descriptor: &OperationDescriptor) -> Self {
        let operation = match descriptor {
            OperationDescriptor::Cli(operation) => operation.local_usage_operation(),
            OperationDescriptor::Mcp(operation) => operation
                .product_operation()
                .map(ObservedMcpProductOperation::local_usage_operation),
            OperationDescriptor::Daemon(_) => None,
        };
        Self {
            operation,
            result_count: 0,
            output_bytes: 0,
            search_context: None,
            value_class: ValueClass::NotApplicable,
        }
    }

    pub fn excluded() -> Self {
        Self {
            operation: None,
            result_count: 0,
            output_bytes: 0,
            search_context: None,
            value_class: ValueClass::NotApplicable,
        }
    }

    /// Accepts only bounded numeric observations from the canonical result.
    ///
    /// `content_bytes` is intentionally ignored by definitions 2 and 3. Historical
    /// adapters supplied JSON framing or non-search payload bytes here; only
    /// the dedicated complete-search observation can populate context facts.
    pub fn set_result_observation(
        &mut self,
        _action: ResultObservationAction,
        result_count: usize,
        _content_bytes: usize,
    ) {
        self.result_count = result_count;
        self.value_class = if result_count == 0 {
            ValueClass::Empty
        } else {
            ValueClass::ResultBearing
        };
    }

    pub fn set_measured_output_bytes(&mut self, output_bytes: usize) {
        self.output_bytes = output_bytes;
    }

    pub fn set_search_context_observation(&mut self, observation: SearchContextObservation) {
        self.search_context = Some(observation);
    }

    pub fn completed(self, success: bool, duration: Duration) -> Option<CompletedOperation> {
        let operation = self.operation?;
        let mut completed = CompletedOperation::cli(operation, success, duration);
        completed.delivered_output_bytes = u64::try_from(self.output_bytes).unwrap_or(u64::MAX);
        if !success {
            return Some(completed);
        }
        if operation == LocalUsageOperation::Search {
            completed.value_class = self.value_class;
            completed.result_count = u64::try_from(self.result_count).unwrap_or(u64::MAX);
        }
        if operation == LocalUsageOperation::Search && self.value_class == ValueClass::ResultBearing
        {
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

pub fn record_best_effort(
    authority: &LocalUsageStorageAuthority,
    control: &UsageControlSnapshot,
    complete: impl FnOnce() -> Option<CompletedOperation>,
) {
    if !control.enabled() {
        return;
    }
    if let Some(operation) = complete() {
        let _ = store::record_authorized(authority, operation);
    }
}

#[derive(Debug, Clone, Default)]
pub struct McpCompletionFacts {
    pub failed: bool,
    pub result_count: Option<usize>,
    pub delivered_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct McpToolUsageFacts {
    pub search_context: Option<SearchContextObservation>,
}

#[derive(Debug, Clone)]
pub struct McpInvocation {
    operation: LocalUsageOperation,
    search_context: Option<SearchContextObservation>,
}

impl McpInvocation {
    pub fn from_operation(operation: ObservedMcpProductOperation) -> Self {
        Self {
            operation: operation.local_usage_operation(),
            search_context: None,
        }
    }

    pub fn blame() -> Self {
        Self {
            operation: LocalUsageOperation::Blame,
            search_context: None,
        }
    }

    pub fn bind_search_context(&mut self, observation: SearchContextObservation) {
        if self.operation == LocalUsageOperation::Search {
            self.search_context = Some(observation);
        }
    }

    pub fn bind_tool_usage(&mut self, usage: McpToolUsageFacts) {
        if self.operation == LocalUsageOperation::Search {
            self.search_context = usage.search_context;
        }
    }

    pub fn completed(&self, facts: &McpCompletionFacts, duration: Duration) -> CompletedOperation {
        let failed = facts.failed;
        let mut operation = CompletedOperation {
            definition_version: DEFINITION_VERSION,
            surface: Surface::Mcp,
            operation: self.operation,
            outcome: if failed {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            value_class: ValueClass::NotApplicable,
            duration: DurationBucket::from_duration(duration),
            context_coverage: ContextCoverage::NotApplicable,
            result_count: 0,
            delivered_output_bytes: u64::try_from(facts.delivered_output_bytes).unwrap_or(u64::MAX),
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        };
        if failed {
            return operation;
        }
        if let Some(count) = facts.result_count {
            operation.value_class = if count == 0 {
                ValueClass::Empty
            } else {
                ValueClass::ResultBearing
            };
            operation.result_count = u64::try_from(count).unwrap_or(u64::MAX);
        }
        if self.operation == LocalUsageOperation::Search
            && operation.value_class == ValueClass::ResultBearing
        {
            let observation = self
                .search_context
                .unwrap_or_else(SearchContextObservation::unavailable);
            operation.context_coverage = observation.coverage;
            operation.delivered_context_bytes = observation.delivered_context_bytes;
            operation.matched_normalized_session_bytes =
                observation.matched_normalized_session_bytes;
        }
        operation
    }
}

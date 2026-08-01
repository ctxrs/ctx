#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputObservationKind {
    Command,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputOutcomeMetadata {
    pub outcome: OutputOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
}

//! Content-free terminal facts. These are measurements, not an upload contract.
//! No callback receives command arguments, content, errors, paths or identifiers.
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operation {
    Unknown,
    Help,
    Version,
    Run,
    Proxy,
    Compact,
    Restore,
    Filter,
    Read,
    Json,
    Summary,
    Errors,
    Test,
    Recall,
    Gain,
    Config,
    Semantic,
    Discover,
    Usage,
    Rewrite,
    Hook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Entry {
    Direct,
    CompletionHook,
    PreHook,
    JsonProtocol,
    PiSessionV1,
    PiSessionV2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Terminal {
    Invocation,
    ProtocolRequest,
    ProtocolSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Host {
    Claude,
    Copilot,
    Hermes,
    Codex,
    Cursor,
    Gemini,
    VsCode,
    Droid,
    Vibe,
    Pi,
    Omp,
    OpenCode,
    Kilo,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    Automatic,
    Capture,
    Raw,
    ExplicitView,
    Restore,
    Rewrite,
    Control,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Outcome {
    Success,
    Skipped,
    FailOpen,
    Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    Arguments,
    Settings,
    Input,
    ProcessSetup,
    Spawn,
    ChildRead,
    ChildWait,
    Codec,
    Render,
    Protocol,
    Output,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureKind {
    InvalidInput,
    NotFound,
    PermissionDenied,
    BrokenPipe,
    Interrupted,
    TimedOut,
    Io,
    Tokenizer,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Failure {
    pub phase: Phase,
    pub kind: FailureKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SkipReason {
    ExplicitRaw,
    Disabled,
    Excluded,
    SettingsUnavailable,
    Interactive,
    Small,
    Binary,
    StreamingDeadline,
    CaptureLimit,
    EnvelopeLimit,
    UnsupportedHost,
    UnsupportedTool,
    UnsupportedEvent,
    UnsupportedShell,
    UnsupportedSyntax,
    UnsupportedMetadata,
    MalformedInput,
    AlreadyWrapped,
    TokenizerUnavailable,
    NotSmaller,
    NoSelection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Missingness {
    Inherited,
    Streaming,
    Small,
    Binary,
    ViewNotTokenized,
    TokenizerUnavailable,
    Incomplete,
    NotApplicable,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChildOutcome {
    NotApplicable,
    Unknown,
    ExitedZero,
    ExitedNonzero,
    Signalled,
    Cancelled,
    SpawnNotFound,
    SpawnDenied,
    SpawnFailed,
}

/// Flushed means the writer accepted the complete payload. Hook/protocol output
/// is not evidence that the host applied the replacement or a model consumed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Delivery {
    NotApplicable,
    NotAttempted,
    Flushed,
    /// A no-op hook envelope; no content was emitted by Sift.
    Unchanged,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Presentation {
    Raw,
    Codec,
    CommandView,
    ExplicitView,
    SemanticSelection,
    Restored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Encoding {
    Raw,
    Json,
    JsonRows,
    JsonMin,
    JsonColumns,
    TextRuns,
    TextPrefixes,
    TextRefs,
    TextLines,
    TextSymbols,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenCounts {
    pub input: u64,
    pub emitted: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamFacts {
    /// Bytes actually read, possibly only a prefix when input_complete is false.
    pub input_bytes: Option<u64>,
    /// Content bytes successfully written, excluding hook/protocol envelopes.
    /// None for an unflushed envelope: partial JSON is not presented text.
    pub emitted_bytes: Option<u64>,
    pub input_complete: bool,
    pub output_complete: bool,
    /// Comparable original/final counts, only for completely delivered text.
    pub tokens: Option<TokenCounts>,
    pub missing: Option<Missingness>,
    pub presentation: Presentation,
    pub encoding: Option<Encoding>,
    pub skip: Option<SkipReason>,
}

impl Default for StreamFacts {
    fn default() -> Self {
        Self {
            input_bytes: None,
            emitted_bytes: None,
            input_complete: false,
            output_complete: false,
            tokens: None,
            missing: Some(Missingness::Unknown),
            presentation: Presentation::Raw,
            encoding: None,
            skip: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticMode {
    Off,
    Shadow,
    Select,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticDisposition {
    Off,
    ProjectNotAllowed,
    InvalidSelection,
    OutsideProject,
    Fallback,
    Rejected,
    Marginal,
    NotSmaller,
    ShadowSelected,
    StorageUnavailable,
    Selected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderOutcome {
    NotAttempted,
    MissingCredential,
    Oversized,
    Unavailable,
    HttpFailure,
    InvalidResponse,
    Success,
    Memoized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HttpClass {
    Informational,
    Success,
    Redirect,
    ClientError,
    ServerError,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticFacts {
    pub mode: SemanticMode,
    pub disposition: SemanticDisposition,
    pub provider: ProviderOutcome,
    pub request_attempted: bool,
    pub cache_hit: bool,
    pub http_class: Option<HttpClass>,
    /// Usage for this request only; memoized historical usage is not copied.
    pub request_input_tokens: Option<u64>,
    pub request_output_tokens: Option<u64>,
    pub provider_duration: Option<Duration>,
    pub passages: Option<u64>,
    pub selected: Option<u64>,
    pub omitted: Option<u64>,
    pub ordinary_tokens: Option<u64>,
    /// Candidate measurement, not delivered savings. Inspect stream delivery.
    pub candidate_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocalRecordOutcome {
    Disabled,
    Recorded,
    Busy,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SiftObservation {
    pub terminal: Terminal,
    pub operation: Operation,
    pub entry: Entry,
    pub host: Option<Host>,
    pub mode: Mode,
    pub outcome: Outcome,
    pub failure: Option<Failure>,
    pub skip: Option<SkipReason>,
    pub child: ChildOutcome,
    pub delivery: Delivery,
    /// stdout/text first, stderr second. Missing fields remain None.
    pub streams: [Option<StreamFacts>; 2],
    pub duration: Duration,
    pub transform_duration: Option<Duration>,
    pub output_duration: Option<Duration>,
    pub local_record: Option<LocalRecordOutcome>,
    pub semantic: Option<SemanticFacts>,
}

impl Default for SiftObservation {
    fn default() -> Self {
        Self {
            terminal: Terminal::Invocation,
            operation: Operation::Unknown,
            entry: Entry::Direct,
            host: None,
            mode: Mode::Automatic,
            outcome: Outcome::Success,
            failure: None,
            skip: None,
            child: ChildOutcome::NotApplicable,
            delivery: Delivery::NotApplicable,
            streams: [None, None],
            duration: Duration::ZERO,
            transform_duration: None,
            output_duration: None,
            local_record: None,
            semantic: None,
        }
    }
}

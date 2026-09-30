//! Exhaustive projections into the collector-owned closed vocabulary.
use ctx_client_observability::analytics as wire;
use ctx_sift::observation as native;

pub(super) fn operation(value: native::Operation) -> wire::SiftOperation {
    match value {
        native::Operation::Unknown => wire::SiftOperation::Unknown,
        native::Operation::Help => wire::SiftOperation::Help,
        native::Operation::Version => wire::SiftOperation::Version,
        native::Operation::Run => wire::SiftOperation::Run,
        native::Operation::Proxy => wire::SiftOperation::Proxy,
        native::Operation::Compact => wire::SiftOperation::Compact,
        native::Operation::Restore => wire::SiftOperation::Restore,
        native::Operation::Filter => wire::SiftOperation::Filter,
        native::Operation::Read => wire::SiftOperation::Read,
        native::Operation::Json => wire::SiftOperation::Json,
        native::Operation::Summary => wire::SiftOperation::Summary,
        native::Operation::Errors => wire::SiftOperation::Errors,
        native::Operation::Test => wire::SiftOperation::Test,
        native::Operation::Recall => wire::SiftOperation::Recall,
        native::Operation::Gain => wire::SiftOperation::Gain,
        native::Operation::Config => wire::SiftOperation::Config,
        native::Operation::Semantic => wire::SiftOperation::Semantic,
        native::Operation::Discover => wire::SiftOperation::Discover,
        native::Operation::Usage => wire::SiftOperation::Usage,
        native::Operation::Rewrite => wire::SiftOperation::Rewrite,
        native::Operation::Hook => wire::SiftOperation::Hook,
    }
}

pub(super) fn entry(value: native::Entry) -> wire::SiftEntry {
    match value {
        native::Entry::Direct => wire::SiftEntry::Direct,
        native::Entry::CompletionHook => wire::SiftEntry::CompletionHook,
        native::Entry::PreHook => wire::SiftEntry::PreHook,
        native::Entry::JsonProtocol => wire::SiftEntry::JsonProtocol,
        native::Entry::PiSessionV1 => wire::SiftEntry::PiSessionV1,
        native::Entry::PiSessionV2 => wire::SiftEntry::PiSessionV2,
    }
}

pub(super) fn terminal(value: native::Terminal) -> wire::SiftTerminal {
    match value {
        native::Terminal::Invocation => wire::SiftTerminal::Invocation,
        native::Terminal::ProtocolRequest => wire::SiftTerminal::ProtocolRequest,
        native::Terminal::ProtocolSession => wire::SiftTerminal::ProtocolSession,
    }
}

pub(super) fn host(value: native::Host) -> wire::SiftHost {
    match value {
        native::Host::Claude => wire::SiftHost::Claude,
        native::Host::Copilot => wire::SiftHost::Copilot,
        native::Host::Hermes => wire::SiftHost::Hermes,
        native::Host::Codex => wire::SiftHost::Codex,
        native::Host::Cursor => wire::SiftHost::Cursor,
        native::Host::Gemini => wire::SiftHost::Gemini,
        native::Host::VsCode => wire::SiftHost::Vscode,
        native::Host::Droid => wire::SiftHost::Droid,
        native::Host::Vibe => wire::SiftHost::Vibe,
        native::Host::Pi => wire::SiftHost::Pi,
        native::Host::Omp => wire::SiftHost::Omp,
        native::Host::OpenCode => wire::SiftHost::OpenCode,
        native::Host::Kilo => wire::SiftHost::Kilo,
        native::Host::Unknown => wire::SiftHost::Unknown,
    }
}

pub(super) fn mode(value: native::Mode) -> wire::SiftMode {
    match value {
        native::Mode::Automatic => wire::SiftMode::Automatic,
        native::Mode::Capture => wire::SiftMode::Capture,
        native::Mode::Raw => wire::SiftMode::Raw,
        native::Mode::ExplicitView => wire::SiftMode::ExplicitView,
        native::Mode::Restore => wire::SiftMode::Restore,
        native::Mode::Rewrite => wire::SiftMode::Rewrite,
        native::Mode::Control => wire::SiftMode::Control,
    }
}

pub(super) fn outcome(value: native::Outcome) -> wire::SiftTerminalOutcome {
    match value {
        native::Outcome::Success => wire::SiftTerminalOutcome::Success,
        native::Outcome::Skipped => wire::SiftTerminalOutcome::Skipped,
        native::Outcome::FailOpen => wire::SiftTerminalOutcome::FailOpen,
        native::Outcome::Failure => wire::SiftTerminalOutcome::Failure,
    }
}

pub(super) fn phase(value: native::Phase) -> wire::SiftPhase {
    match value {
        native::Phase::Arguments => wire::SiftPhase::Arguments,
        native::Phase::Settings => wire::SiftPhase::Settings,
        native::Phase::Input => wire::SiftPhase::Input,
        native::Phase::ProcessSetup => wire::SiftPhase::ProcessSetup,
        native::Phase::Spawn => wire::SiftPhase::Spawn,
        native::Phase::ChildRead => wire::SiftPhase::ChildRead,
        native::Phase::ChildWait => wire::SiftPhase::ChildWait,
        native::Phase::Codec => wire::SiftPhase::Codec,
        native::Phase::Render => wire::SiftPhase::Render,
        native::Phase::Protocol => wire::SiftPhase::Protocol,
        native::Phase::Output => wire::SiftPhase::Output,
        native::Phase::Other => wire::SiftPhase::Other,
    }
}

pub(super) fn failure_kind(value: native::FailureKind) -> wire::SiftFailureKind {
    match value {
        native::FailureKind::InvalidInput => wire::SiftFailureKind::InvalidInput,
        native::FailureKind::NotFound => wire::SiftFailureKind::NotFound,
        native::FailureKind::PermissionDenied => wire::SiftFailureKind::PermissionDenied,
        native::FailureKind::BrokenPipe => wire::SiftFailureKind::BrokenPipe,
        native::FailureKind::Interrupted => wire::SiftFailureKind::Interrupted,
        native::FailureKind::TimedOut => wire::SiftFailureKind::TimedOut,
        native::FailureKind::Io => wire::SiftFailureKind::Io,
        native::FailureKind::Tokenizer => wire::SiftFailureKind::Tokenizer,
        native::FailureKind::Other => wire::SiftFailureKind::Other,
    }
}

pub(super) fn skip_reason(value: native::SkipReason) -> wire::SiftSkipReason {
    match value {
        native::SkipReason::ExplicitRaw => wire::SiftSkipReason::ExplicitRaw,
        native::SkipReason::Disabled => wire::SiftSkipReason::Disabled,
        native::SkipReason::Excluded => wire::SiftSkipReason::Excluded,
        native::SkipReason::SettingsUnavailable => wire::SiftSkipReason::SettingsUnavailable,
        native::SkipReason::Interactive => wire::SiftSkipReason::Interactive,
        native::SkipReason::Small => wire::SiftSkipReason::Small,
        native::SkipReason::Binary => wire::SiftSkipReason::Binary,
        native::SkipReason::StreamingDeadline => wire::SiftSkipReason::StreamingDeadline,
        native::SkipReason::CaptureLimit => wire::SiftSkipReason::CaptureLimit,
        native::SkipReason::EnvelopeLimit => wire::SiftSkipReason::EnvelopeLimit,
        native::SkipReason::UnsupportedHost => wire::SiftSkipReason::UnsupportedHost,
        native::SkipReason::UnsupportedTool => wire::SiftSkipReason::UnsupportedTool,
        native::SkipReason::UnsupportedEvent => wire::SiftSkipReason::UnsupportedEvent,
        native::SkipReason::UnsupportedShell => wire::SiftSkipReason::UnsupportedShell,
        native::SkipReason::UnsupportedSyntax => wire::SiftSkipReason::UnsupportedSyntax,
        native::SkipReason::UnsupportedMetadata => wire::SiftSkipReason::UnsupportedMetadata,
        native::SkipReason::MalformedInput => wire::SiftSkipReason::MalformedInput,
        native::SkipReason::AlreadyWrapped => wire::SiftSkipReason::AlreadyWrapped,
        native::SkipReason::TokenizerUnavailable => wire::SiftSkipReason::TokenizerUnavailable,
        native::SkipReason::NotSmaller => wire::SiftSkipReason::NotSmaller,
        native::SkipReason::NoSelection => wire::SiftSkipReason::NoSelection,
    }
}

pub(super) fn missingness(value: native::Missingness) -> wire::SiftMissingness {
    match value {
        native::Missingness::Inherited => wire::SiftMissingness::Inherited,
        native::Missingness::Streaming => wire::SiftMissingness::Streaming,
        native::Missingness::Small => wire::SiftMissingness::Small,
        native::Missingness::Binary => wire::SiftMissingness::Binary,
        native::Missingness::ViewNotTokenized => wire::SiftMissingness::ViewNotTokenized,
        native::Missingness::TokenizerUnavailable => wire::SiftMissingness::TokenizerUnavailable,
        native::Missingness::Incomplete => wire::SiftMissingness::Incomplete,
        native::Missingness::NotApplicable => wire::SiftMissingness::NotApplicable,
        native::Missingness::Unknown => wire::SiftMissingness::Unknown,
    }
}

pub(super) fn child_outcome(value: native::ChildOutcome) -> wire::SiftChildOutcome {
    match value {
        native::ChildOutcome::NotApplicable => wire::SiftChildOutcome::NotApplicable,
        native::ChildOutcome::Unknown => wire::SiftChildOutcome::Unknown,
        native::ChildOutcome::ExitedZero => wire::SiftChildOutcome::ExitedZero,
        native::ChildOutcome::ExitedNonzero => wire::SiftChildOutcome::ExitedNonzero,
        native::ChildOutcome::Signalled => wire::SiftChildOutcome::Signalled,
        native::ChildOutcome::Cancelled => wire::SiftChildOutcome::Cancelled,
        native::ChildOutcome::SpawnNotFound => wire::SiftChildOutcome::SpawnNotFound,
        native::ChildOutcome::SpawnDenied => wire::SiftChildOutcome::SpawnDenied,
        native::ChildOutcome::SpawnFailed => wire::SiftChildOutcome::SpawnFailed,
    }
}

pub(super) fn delivery(value: native::Delivery) -> wire::SiftDelivery {
    match value {
        native::Delivery::NotApplicable => wire::SiftDelivery::NotApplicable,
        native::Delivery::NotAttempted => wire::SiftDelivery::NotAttempted,
        native::Delivery::Flushed => wire::SiftDelivery::Flushed,
        native::Delivery::Unchanged => wire::SiftDelivery::Unchanged,
        native::Delivery::Failed => wire::SiftDelivery::Failed,
    }
}

pub(super) fn semantic_mode(value: native::SemanticMode) -> wire::SiftSemanticMode {
    match value {
        native::SemanticMode::Off => wire::SiftSemanticMode::Off,
        native::SemanticMode::Shadow => wire::SiftSemanticMode::Shadow,
        native::SemanticMode::Select => wire::SiftSemanticMode::Select,
    }
}

pub(super) fn semantic_disposition(
    value: native::SemanticDisposition,
) -> wire::SiftSemanticDisposition {
    match value {
        native::SemanticDisposition::Off => wire::SiftSemanticDisposition::Off,
        native::SemanticDisposition::ProjectNotAllowed => {
            wire::SiftSemanticDisposition::ProjectNotAllowed
        }
        native::SemanticDisposition::InvalidSelection => {
            wire::SiftSemanticDisposition::InvalidSelection
        }
        native::SemanticDisposition::OutsideProject => {
            wire::SiftSemanticDisposition::OutsideProject
        }
        native::SemanticDisposition::Fallback => wire::SiftSemanticDisposition::Fallback,
        native::SemanticDisposition::Rejected => wire::SiftSemanticDisposition::Rejected,
        native::SemanticDisposition::Marginal => wire::SiftSemanticDisposition::Marginal,
        native::SemanticDisposition::NotSmaller => wire::SiftSemanticDisposition::NotSmaller,
        native::SemanticDisposition::ShadowSelected => {
            wire::SiftSemanticDisposition::ShadowSelected
        }
        native::SemanticDisposition::StorageUnavailable => {
            wire::SiftSemanticDisposition::StorageUnavailable
        }
        native::SemanticDisposition::Selected => wire::SiftSemanticDisposition::Selected,
    }
}

pub(super) fn provider_outcome(value: native::ProviderOutcome) -> wire::SiftProviderOutcome {
    match value {
        native::ProviderOutcome::NotAttempted => wire::SiftProviderOutcome::NotAttempted,
        native::ProviderOutcome::MissingCredential => wire::SiftProviderOutcome::MissingCredential,
        native::ProviderOutcome::Oversized => wire::SiftProviderOutcome::Oversized,
        native::ProviderOutcome::Unavailable => wire::SiftProviderOutcome::Unavailable,
        native::ProviderOutcome::HttpFailure => wire::SiftProviderOutcome::HttpFailure,
        native::ProviderOutcome::InvalidResponse => wire::SiftProviderOutcome::InvalidResponse,
        native::ProviderOutcome::Success => wire::SiftProviderOutcome::Success,
        native::ProviderOutcome::Memoized => wire::SiftProviderOutcome::Memoized,
    }
}

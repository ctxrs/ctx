use super::*;
vocabulary!(SiftEntry { Mcp => "mcp",
    Direct => "direct", CompletionHook => "completion_hook", PreHook => "pre_hook", JsonProtocol => "json_protocol", PiSessionV1 => "pi_session_v1", PiSessionV2 => "pi_session_v2"
});
vocabulary!(SiftTerminal {
    Invocation => "invocation", ProtocolRequest => "protocol_request", ProtocolSession => "protocol_session"
});
vocabulary!(SiftTerminalOutcome {
    Success => "success", Skipped => "skipped", FailOpen => "fail_open", Failure => "failure"
});
vocabulary!(SiftPhase {
    Arguments => "arguments", Settings => "settings", Input => "input", ProcessSetup => "process_setup", Spawn => "spawn", ChildRead => "child_read", ChildWait => "child_wait", Codec => "codec", Render => "render", Protocol => "protocol", Output => "output", Other => "other"
});
vocabulary!(SiftFailureKind {
    InvalidInput => "invalid_input", NotFound => "not_found", PermissionDenied => "permission_denied", BrokenPipe => "broken_pipe", Interrupted => "interrupted", TimedOut => "timed_out", Io => "io", Tokenizer => "tokenizer", Other => "other"
});
vocabulary!(SiftSkipReason {
    ExplicitRaw => "explicit_raw", Disabled => "disabled", Excluded => "excluded", SettingsUnavailable => "settings_unavailable", Interactive => "interactive", Small => "small", Binary => "binary", StreamingDeadline => "streaming_deadline", CaptureLimit => "capture_limit", EnvelopeLimit => "envelope_limit", UnsupportedHost => "unsupported_host", UnsupportedTool => "unsupported_tool", UnsupportedEvent => "unsupported_event", UnsupportedShell => "unsupported_shell", UnsupportedSyntax => "unsupported_syntax", UnsupportedMetadata => "unsupported_metadata", MalformedInput => "malformed_input", AlreadyWrapped => "already_wrapped", TokenizerUnavailable => "tokenizer_unavailable", NotSmaller => "not_smaller", NoSelection => "no_selection"
});
vocabulary!(SiftMissingness {
    Inherited => "inherited", Streaming => "streaming", Small => "small", Binary => "binary", ViewNotTokenized => "view_not_tokenized", TokenizerUnavailable => "tokenizer_unavailable", Incomplete => "incomplete", NotApplicable => "not_applicable", Unknown => "unknown"
});
vocabulary!(SiftChildOutcome {
    NotApplicable => "not_applicable", Unknown => "unknown", ExitedZero => "exited_zero", ExitedNonzero => "exited_nonzero", Signalled => "signalled", Cancelled => "cancelled", SpawnNotFound => "spawn_not_found", SpawnDenied => "spawn_denied", SpawnFailed => "spawn_failed"
});
vocabulary!(SiftDelivery {
    NotApplicable => "not_applicable", NotAttempted => "not_attempted", Flushed => "flushed", Unchanged => "unchanged", Failed => "failed"
});
vocabulary!(SiftSemanticMode {
    Off => "off", Shadow => "shadow", Select => "select"
});
vocabulary!(SiftSemanticDisposition {
    Off => "off", ProjectNotAllowed => "project_not_allowed", InvalidSelection => "invalid_selection", OutsideProject => "outside_project", Fallback => "fallback", Rejected => "rejected", Marginal => "marginal", NotSmaller => "not_smaller", ShadowSelected => "shadow_selected", StorageUnavailable => "storage_unavailable", Selected => "selected"
});
vocabulary!(SiftProviderOutcome {
    NotAttempted => "not_attempted", MissingCredential => "missing_credential", Oversized => "oversized", Unavailable => "unavailable", HttpFailure => "http_failure", InvalidResponse => "invalid_response", Success => "success", Memoized => "memoized"
});

/// Every summary represents a single fixed cohort. Stream reasons are projected
/// only when they agree; mixed reasons remain None, never a fabricated reason.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiftCohort {
    pub entry: Option<SiftEntry>,
    pub terminal: Option<SiftTerminal>,
    pub outcome: Option<SiftTerminalOutcome>,
    pub delivery: Option<SiftDelivery>,
    pub skip: Option<SiftSkipReason>,
    pub failure_phase: Option<SiftPhase>,
    pub failure_kind: Option<SiftFailureKind>,
    pub child: Option<SiftChildOutcome>,
    pub missingness: Option<SiftMissingness>,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeasuredTotal {
    pub samples: u64,
    pub total: u64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiftSemanticSummary {
    pub mode: SiftSemanticMode,
    pub disposition: SiftSemanticDisposition,
    pub provider: SiftProviderOutcome,
    pub observed: u64,
    pub requests_attempted: u64,
    pub cache_hits: u64,
    /// Actual current-request usage only; memoized earlier usage is excluded.
    pub input_tokens: Option<MeasuredTotal>,
    pub output_tokens: Option<MeasuredTotal>,
    pub provider_latency: Option<[u64; 14]>,
}

impl SiftCohort {
    pub(super) fn insert(self, p: &mut Map<String, Value>) {
        if let Some(v) = self.entry {
            p.insert("sift_entry".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.terminal {
            p.insert("sift_terminal".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.outcome {
            p.insert("sift_outcome".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.delivery {
            p.insert("sift_delivery".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.skip {
            p.insert("sift_skip".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.failure_phase {
            p.insert("sift_failure_phase".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.failure_kind {
            p.insert("sift_failure_kind".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.child {
            p.insert("sift_child".into(), serde_json::json!(v.as_str()));
        }
        if let Some(v) = self.missingness {
            p.insert("sift_missingness".into(), serde_json::json!(v.as_str()));
        }
    }
}
impl SiftSemanticSummary {
    pub(super) fn insert(self, p: &mut Map<String, Value>) {
        for (key, value) in [
            ("sift_semantic_mode", self.mode.as_str()),
            ("sift_semantic_disposition", self.disposition.as_str()),
            ("sift_semantic_provider", self.provider.as_str()),
        ] {
            p.insert(key.into(), serde_json::json!(value));
        }
        for (key, value) in [
            ("sift_semantic_observed_count_bucket", self.observed),
            (
                "sift_semantic_requests_count_bucket",
                self.requests_attempted,
            ),
            ("sift_semantic_cache_hits_count_bucket", self.cache_hits),
        ] {
            wire::count(p, key, Some(value));
        }
        windows::total(p, "sift_provider_input_tokens", self.input_tokens, false);
        windows::total(p, "sift_provider_output_tokens", self.output_tokens, false);
        windows::histogram(p, "sift_provider_", self.provider_latency);
    }
}

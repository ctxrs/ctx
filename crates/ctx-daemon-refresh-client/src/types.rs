use super::*;

#[derive(Debug)]
pub struct SourceBackedRefreshDaemonUnavailable {
    detail: Option<String>,
}

impl SourceBackedRefreshDaemonUnavailable {
    #[doc(hidden)]
    pub fn new(detail: Option<String>) -> Self {
        Self { detail }
    }
}

impl fmt::Display for SourceBackedRefreshDaemonUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the ctx daemon is unavailable for source-backed refresh")?;
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        formatter.write_str("; no foreground writer was started")
    }
}

impl std::error::Error for SourceBackedRefreshDaemonUnavailable {}

#[derive(Debug)]
pub struct SourceBackedRefreshPendingPublication {
    request_id: String,
    request_state: String,
    source_count: usize,
}

impl SourceBackedRefreshPendingPublication {
    pub fn new(request_id: String, request_state: String, source_count: usize) -> Self {
        Self {
            request_id,
            request_state,
            source_count,
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn request_state(&self) -> &str {
        &self.request_state
    }

    pub fn source_count(&self) -> usize {
        self.source_count
    }
}

impl fmt::Display for SourceBackedRefreshPendingPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "daemon source refresh was queued but no published generation exists; retry with --refresh wait",
        )
    }
}

impl std::error::Error for SourceBackedRefreshPendingPublication {}

#[allow(dead_code)] // Request metadata is retained for CLI/status integrations.
pub struct SourceBackedRefreshObservation {
    pub mode: SourceBackedRefreshMode,
    pub status: String,
    pub request_id: Option<String>,
    pub daemon_available: bool,
    pub source_count: usize,
    pub request_previous_generation: Option<String>,
    pub request_generation_changed: bool,
    pub scanned_routes: Option<usize>,
    pub receipt: Option<SourceBackedRefreshReceipt>,
    pub pin: PinnedSourceBackedGeneration,
}

#[derive(Debug)]
pub struct SourceBackedRefreshTerminalError {
    pub code: String,
    pub class: String,
    pub retryable: bool,
    pub affected_routes: Vec<String>,
    pub retryable_routes: Vec<String>,
    pub blocked_routes: Vec<String>,
    pub physical_attempt_id: String,
    pub retained_generation: Option<String>,
    pub retry_advice: Option<String>,
    detail: Option<String>,
    capture_error: Option<CaptureError>,
}

impl fmt::Display for SourceBackedRefreshTerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon-owned source-backed refresh failed (code={}, class={}, retryable={}, attempt={}",
            self.code, self.class, self.retryable, self.physical_attempt_id
        )?;
        write!(
            formatter,
            ", affected_routes={:?}, retryable_routes={:?}, blocked_routes={:?}",
            self.affected_routes, self.retryable_routes, self.blocked_routes
        )?;
        write!(
            formatter,
            ", retained_generation={:?}, retry_advice={:?})",
            self.retained_generation, self.retry_advice
        )?;
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SourceBackedRefreshTerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.capture_error
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

impl From<RefreshTerminalOutcome> for SourceBackedRefreshTerminalError {
    fn from(outcome: RefreshTerminalOutcome) -> Self {
        let capture_error = match outcome.class {
            RefreshOutcomeClass::Incompatible => Some(CaptureError::UnsupportedSchema(
                outcome
                    .detail
                    .clone()
                    .unwrap_or_else(|| outcome.code.as_str().to_owned()),
            )),
            RefreshOutcomeClass::Unreadable => Some(CaptureError::InvalidPayload(
                outcome
                    .detail
                    .clone()
                    .unwrap_or_else(|| outcome.code.as_str().to_owned()),
            )),
            _ => None,
        };
        Self {
            code: outcome.code.as_str().to_owned(),
            class: outcome.class.as_str().to_owned(),
            retryable: outcome.retryable,
            affected_routes: outcome
                .affected_routes
                .into_iter()
                .map(|route| route.as_str().to_owned())
                .collect(),
            retryable_routes: outcome
                .retryable_routes
                .into_iter()
                .map(|route| route.as_str().to_owned())
                .collect(),
            blocked_routes: outcome
                .blocked_routes
                .into_iter()
                .map(|route| route.as_str().to_owned())
                .collect(),
            physical_attempt_id: outcome.physical_attempt_id,
            retained_generation: outcome.retained_generation,
            retry_advice: outcome
                .retry_advice
                .map(|advice| advice.as_str().to_owned()),
            detail: outcome.detail,
            capture_error,
        }
    }
}

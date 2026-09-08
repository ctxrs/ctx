use super::*;

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
    outcome: RefreshTerminalOutcome,
    capture_error: Option<CaptureError>,
}

impl SourceBackedRefreshTerminalError {
    pub fn outcome(&self) -> &RefreshTerminalOutcome {
        &self.outcome
    }
}

impl fmt::Display for SourceBackedRefreshTerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let outcome = self.outcome();
        let affected_routes = outcome
            .affected_routes()
            .iter()
            .map(|route| route.as_str())
            .collect::<Vec<_>>();
        let retryable_routes = outcome
            .retryable_routes()
            .iter()
            .map(|route| route.as_str())
            .collect::<Vec<_>>();
        let blocked_routes = outcome
            .blocked_routes()
            .iter()
            .map(|route| route.as_str())
            .collect::<Vec<_>>();
        write!(
            formatter,
            "daemon-owned source-backed refresh failed (code={}, class={}, retryable={}, attempt={}",
            outcome.code().as_str(),
            outcome.class().as_str(),
            outcome.retryable(),
            outcome.physical_attempt_id()
        )?;
        write!(
            formatter,
            ", affected_routes={:?}, retryable_routes={:?}, blocked_routes={:?}",
            affected_routes, retryable_routes, blocked_routes
        )?;
        write!(
            formatter,
            ", retained_generation={:?}, retry_advice={:?})",
            outcome.retained_generation(),
            outcome.retry_advice().map(|advice| advice.as_str())
        )?;
        if let Some(detail) = outcome.detail() {
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
        let capture_error = match outcome.class() {
            RefreshOutcomeClass::Incompatible => Some(CaptureError::UnsupportedSchema(
                outcome
                    .detail()
                    .map(str::to_owned)
                    .unwrap_or_else(|| outcome.code().as_str().to_owned()),
            )),
            RefreshOutcomeClass::Unreadable => Some(CaptureError::InvalidPayload(
                outcome
                    .detail()
                    .map(str::to_owned)
                    .unwrap_or_else(|| outcome.code().as_str().to_owned()),
            )),
            _ => None,
        };
        Self {
            outcome,
            capture_error,
        }
    }
}

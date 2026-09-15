use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{count_bucket, duration_bucket, CountBucket, DurationBucket, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsDeliveryFailureClass {
    None,
    Transport,
    RateLimited,
    ClientRejection,
    Server,
    LocalIo,
    Configuration,
    Unknown,
}

impl AnalyticsDeliveryFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Transport => "transport",
            Self::RateLimited => "rate_limited",
            Self::ClientRejection => "client_rejection",
            Self::Server => "server",
            Self::LocalIo => "local_io",
            Self::Configuration => "configuration",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsDeliveryFailureReason {
    RequestDns,
    RequestConnect,
    RequestTimeout,
    RequestIo,
    ResponseStatus408,
    ResponseBodyTimeout,
    ResponseBodyIo,
    FileOpen,
    FileWrite,
    FileFlush,
    OutboxCorrupt,
    OutboxExpired,
    OutboxCapacity,
    OutboxClock,
    OutboxOversized,
}

impl AnalyticsDeliveryFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestDns => "request_dns",
            Self::RequestConnect => "request_connect",
            Self::RequestTimeout => "request_timeout",
            Self::RequestIo => "request_io",
            Self::ResponseStatus408 => "response_status_408",
            Self::ResponseBodyTimeout => "response_body_timeout",
            Self::ResponseBodyIo => "response_body_io",
            Self::FileOpen => "file_open",
            Self::FileWrite => "file_write",
            Self::FileFlush => "file_flush",
            Self::OutboxCorrupt => "outbox_corrupt",
            Self::OutboxExpired => "outbox_expired",
            Self::OutboxCapacity => "outbox_capacity",
            Self::OutboxClock => "outbox_clock",
            Self::OutboxOversized => "outbox_oversized",
        }
    }

    pub const fn permits(self, class: AnalyticsDeliveryFailureClass) -> bool {
        match self {
            Self::RequestDns
            | Self::RequestConnect
            | Self::RequestTimeout
            | Self::RequestIo
            | Self::ResponseStatus408
            | Self::ResponseBodyTimeout
            | Self::ResponseBodyIo => matches!(class, AnalyticsDeliveryFailureClass::Transport),
            _ => matches!(class, AnalyticsDeliveryFailureClass::LocalIo),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalyticsDeliveryObservationV1 {
    pub queued: CountBucket,
    pub retry_attempts: CountBucket,
    pub dropped: CountBucket,
    pub oldest_queued_age: DurationBucket,
    pub failure_class: AnalyticsDeliveryFailureClass,
    pub failure_reason: Option<AnalyticsDeliveryFailureReason>,
}

impl AnalyticsDeliveryObservationV1 {
    pub fn new(
        queued: u64,
        retry_attempts: u64,
        dropped: u64,
        oldest_queued_age: Duration,
        failure_class: AnalyticsDeliveryFailureClass,
    ) -> Self {
        Self {
            queued: count_bucket(queued),
            retry_attempts: count_bucket(retry_attempts),
            dropped: count_bucket(dropped),
            oldest_queued_age: duration_bucket(oldest_queued_age),
            failure_class,
            failure_reason: None,
        }
    }

    pub fn with_failure_reason(mut self, reason: Option<AnalyticsDeliveryFailureReason>) -> Self {
        self.failure_reason = reason.filter(|reason| reason.permits(self.failure_class));
        self
    }

    pub fn outcome(self) -> Outcome {
        if self.queued == CountBucket::Zero
            && self.dropped == CountBucket::Zero
            && self.failure_class == AnalyticsDeliveryFailureClass::None
        {
            Outcome::Success
        } else {
            Outcome::Failure
        }
    }
}

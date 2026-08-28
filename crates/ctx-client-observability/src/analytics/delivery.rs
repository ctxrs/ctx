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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalyticsDeliveryObservationV1 {
    pub queued: CountBucket,
    pub retry_attempts: CountBucket,
    pub dropped: CountBucket,
    pub oldest_queued_age: DurationBucket,
    pub failure_class: AnalyticsDeliveryFailureClass,
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
        }
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

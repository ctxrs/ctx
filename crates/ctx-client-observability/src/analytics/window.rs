//! Bounded product counters, retired when taken rather than retried as events.
use super::{PublicEventV1, SiftSummaryV1};
use serde::{Deserialize, Serialize};
use std::time::Duration;

mod merge;

const MAX_COHORTS: usize = 32;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "bounded inline cohorts permit atomic checked merges without per-sample allocation"
)]
pub enum Summary {
    Sift(SiftSummaryV1),
    Server(super::ServerSummaryV1),
    Sharing(super::SharingSummaryV1),
}
impl Summary {
    fn event(self) -> Option<PublicEventV1> {
        match self {
            Self::Sift(v) => v.into_event(),
            Self::Server(v) => v.into_event(),
            Self::Sharing(v) => v.into_event(),
        }
    }
    fn limited(&mut self) {
        match self {
            Self::Sift(v) => v.collection_limited = true,
            Self::Server(v) => v.collection_limited = true,
            Self::Sharing(v) => v.collection_limited = true,
        }
    }
    fn window(&mut self, duration: Duration) {
        match self {
            Self::Sift(v) => v.window = duration,
            Self::Server(v) => v.window = duration,
            Self::Sharing(v) => v.window = duration,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Window {
    started_unix_secs: i64,
    limited: bool,
    cohorts: Vec<Summary>,
}
impl Window {
    pub fn new(now: i64) -> Self {
        Self {
            started_unix_secs: now,
            ..Default::default()
        }
    }

    pub fn is_bounded(&self) -> bool {
        self.cohorts.len() <= MAX_COHORTS
    }

    pub fn mark_limited(&mut self) {
        self.limited = true;
    }

    pub fn record(&mut self, next: Summary, now: i64) {
        if next.event().is_none() {
            self.limited = true;
            return;
        }
        if self.cohorts.is_empty() {
            self.started_unix_secs = now;
        }
        if let Some(current) = self
            .cohorts
            .iter_mut()
            .find(|v| merge::same_cohort(**v, next))
        {
            if let Some(merged) = merge::combine(*current, next) {
                *current = merged;
            } else {
                self.limited = true;
            }
        } else if self.cohorts.len() < MAX_COHORTS {
            self.cohorts.push(next);
        } else {
            self.limited = true;
        }
    }
    pub fn take(&mut self, now: i64) -> Vec<PublicEventV1> {
        let duration =
            Duration::from_secs(now.saturating_sub(self.started_unix_secs).max(0) as u64);
        let limited = self.limited;
        let events = std::mem::take(&mut self.cohorts)
            .into_iter()
            .filter_map(|mut v| {
                v.window(duration);
                if limited {
                    v.limited();
                }
                v.event()
            })
            .collect();
        self.started_unix_secs = now;
        self.limited = false;
        events
    }
}

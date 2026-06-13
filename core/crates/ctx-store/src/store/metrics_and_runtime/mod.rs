use super::*;
use ctx_core::boolish::parse_boolish;

pub(super) const DEFAULT_EVENT_LOG_FLUSH_MS: u64 = 250;
pub(super) const DEFAULT_EVENT_LOG_BATCH_SIZE: usize = 256;
pub(super) const DEFAULT_EVENT_LOG_CHECKPOINT_MS: u64 = 5_000;
pub(super) const EVENT_LOG_QUEUE_CAPACITY: usize = 4096;
pub(super) const DEFAULT_ACTIVE_HEAD_PROJECTION_FLUSH_MS: u64 = 50;
pub(super) const ACTIVE_HEAD_PROJECTION_QUEUE_CAPACITY: usize = 4096;

include!("active_head_projection.rs");
include!("event_log.rs");
include!("write_metrics.rs");

use super::support::{RestartFailingAdapter, RestartTrackingAdapter, UnsupportedRestartAdapter};
use super::*;

#[path = "auth_change/cache_invalidation.rs"]
mod cache_invalidation;
#[path = "auth_change/failures.rs"]
mod failures;
#[path = "auth_change/fixtures.rs"]
mod fixtures;
#[path = "auth_change/unsupported.rs"]
mod unsupported;

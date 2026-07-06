use super::{parse_event_window_limit, parse_search_limit, MAX_EVENT_WINDOW, MAX_SEARCH_LIMIT};
use crate::commands::sql::parse_sql_timeout;
use crate::search_filters::parse_since_filter;
use crate::transcript::normalize_uuid_prefix;
use ctx_history_core::utc_now;
use proptest::prelude::*;
use std::panic;

proptest! {
    #[test]
    fn cli_value_parsers_never_panic_for_generated_strings(input in ".{0,256}") {
        prop_assert!(panic::catch_unwind(|| parse_since_filter(&input)).is_ok());
        prop_assert!(panic::catch_unwind(|| parse_search_limit(&input)).is_ok());
        prop_assert!(panic::catch_unwind(|| parse_event_window_limit(&input)).is_ok());
        prop_assert!(panic::catch_unwind(|| parse_sql_timeout(&input)).is_ok());
        prop_assert!(panic::catch_unwind(|| normalize_uuid_prefix(&input, "test")).is_ok());
    }

    #[test]
    fn parse_search_limit_accepts_only_public_limit_range(limit in 1usize..=MAX_SEARCH_LIMIT) {
        prop_assert_eq!(parse_search_limit(&limit.to_string()), Ok(limit));
    }

    #[test]
    fn parse_search_limit_rejects_values_above_public_limit(limit in (MAX_SEARCH_LIMIT + 1)..=usize::MAX) {
        prop_assert!(parse_search_limit(&limit.to_string()).is_err());
    }

    #[test]
    fn parse_event_window_limit_accepts_only_public_window_range(limit in 0usize..=MAX_EVENT_WINDOW) {
        prop_assert_eq!(parse_event_window_limit(&limit.to_string()), Ok(limit));
    }

    #[test]
    fn parse_event_window_limit_rejects_values_above_public_window(limit in (MAX_EVENT_WINDOW + 1)..=usize::MAX) {
        prop_assert!(parse_event_window_limit(&limit.to_string()).is_err());
    }

    #[test]
    fn parse_since_filter_rejects_unrepresentable_day_windows(days in any::<i64>()) {
        if chrono::Duration::try_days(days)
            .and_then(|duration| utc_now().checked_sub_signed(duration))
            .is_none()
        {
            let input = format!("{days}d");
            prop_assert!(parse_since_filter(&input).is_err());
        }
    }
}

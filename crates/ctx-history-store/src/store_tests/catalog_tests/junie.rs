#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v40_adds_junie_provider_checks() {
    assert_provider_migration_accepts(
        39,
        "junie",
        "junie_session_events_jsonl_tree",
        "/tmp/junie/sessions",
        "/tmp/junie/sessions/session-260607-100000-acme/events.jsonl",
    );
}

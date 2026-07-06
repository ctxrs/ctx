#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v30_adds_auggie_provider_checks() {
    assert_provider_migration_accepts(
        29,
        "auggie",
        "auggie_session_json",
        "/tmp/augment/sessions",
        "/tmp/augment/sessions/session.json",
    );
}

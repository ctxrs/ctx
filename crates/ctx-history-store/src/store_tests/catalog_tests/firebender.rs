#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v31_adds_firebender_provider_checks() {
    assert_provider_migration_accepts(
        30,
        "firebender",
        "firebender_chat_history_sqlite",
        "/tmp/project/.idea/firebender/chat_history.db",
        "/tmp/project/.idea/firebender/chat_history.db",
    );
}

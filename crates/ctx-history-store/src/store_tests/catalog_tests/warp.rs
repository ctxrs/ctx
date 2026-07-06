#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v36_adds_warp_provider_checks() {
    assert_provider_migration_accepts(
        35,
        "warp",
        "warp_sqlite",
        "/tmp/warp-terminal",
        "/tmp/warp-terminal/warp.sqlite",
    );
}

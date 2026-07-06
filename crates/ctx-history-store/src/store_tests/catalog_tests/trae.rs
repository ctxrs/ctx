#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v35_adds_trae_provider_checks() {
    assert_provider_migration_accepts(
        34,
        "trae",
        "trae_state_vscdb",
        "/tmp/Trae/User/workspaceStorage",
        "/tmp/Trae/User/workspaceStorage/workspace/state.vscdb",
    );
}

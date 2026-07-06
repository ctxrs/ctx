#[allow(unused_imports)]
use super::*;

pub(crate) fn shared_provider_history_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/provider-history")
        .join(name)
}

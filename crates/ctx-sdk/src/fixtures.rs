use std::path::{Path, PathBuf};

pub fn fixture_path(name: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/agent-history-v1/fixtures")
        .join(name)
}

#[allow(unused_imports)]
use super::*;

pub(crate) fn tempdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("ctx-history-capture-")
        .tempdir()
        .unwrap()
}

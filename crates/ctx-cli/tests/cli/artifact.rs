#[allow(unused_imports)]
use super::*;

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct FakeRelease {
    pub(crate) target: PathBuf,
    pub(crate) metadata: PathBuf,
    pub(crate) signature: PathBuf,
    pub(crate) artifact_sha: String,
}

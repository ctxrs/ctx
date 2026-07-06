#[allow(unused_imports)]
use super::*;

impl BlobWriteGuard {
    pub(crate) fn commit(&mut self) {
        self.committed = true;
        self.created_paths.clear();
    }
}

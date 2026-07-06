#[allow(unused_imports)]
use super::*;

impl Drop for CwdGuard {
    fn drop(&mut self) {
        env::set_current_dir(&self.original).unwrap();
    }
}

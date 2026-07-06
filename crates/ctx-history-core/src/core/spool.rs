#[allow(unused_imports)]
use super::*;

pub fn spool_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("spool")
}

pub fn inbox_dir(root: PathBuf) -> PathBuf {
    spool_dir(root)
}

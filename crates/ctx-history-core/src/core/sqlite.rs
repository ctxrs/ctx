#[allow(unused_imports)]
use super::*;

pub fn database_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("work.sqlite")
}

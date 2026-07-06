#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum FileChangeKind {
        Read => "read",
        Created => "created",
        Modified => "modified",
        Deleted => "deleted",
        Renamed => "renamed",
        Unknown => "unknown",
    }
    default Unknown
}

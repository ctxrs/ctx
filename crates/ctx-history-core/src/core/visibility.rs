#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum Visibility {
        LocalOnly => "local_only",
        Reportable => "reportable",
        SyncMetadata => "sync_metadata",
        SyncFull => "sync_full",
        Withheld => "withheld",
    }
    default LocalOnly
}

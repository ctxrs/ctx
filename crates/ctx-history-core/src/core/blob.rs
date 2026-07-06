#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum SyncOutboxOperation {
        Insert => "insert",
        Update => "update",
        Delete => "delete",
        BlobUpload => "blob_upload",
    }
    default Insert
}

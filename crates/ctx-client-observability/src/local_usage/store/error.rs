#[derive(Debug, thiserror::Error)]
pub enum UsageStoreError {
    #[error("usage store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("usage store SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("usage store has an unsupported application ID")]
    ApplicationId,
    #[error("usage store has unsupported schema version {0}")]
    SchemaVersion(i64),
    #[error("usage store schema does not match its declared version")]
    SchemaIdentity,
    #[error("usage store exceeds its size limit")]
    GrowthLimit,
    #[error("usage store contains inconsistent aggregates")]
    Integrity,
    #[error("usage store date is ahead of the current UTC day")]
    FutureDate,
    #[error("usage store cannot be reported without changing its SQLite file family")]
    UnsafeReadState,
}

impl UsageStoreError {
    pub const fn public_message(&self) -> &'static str {
        match self {
            Self::ApplicationId
            | Self::SchemaVersion(_)
            | Self::SchemaIdentity
            | Self::Integrity => "local usage store format is not supported",
            Self::FutureDate => "local usage store date is ahead of the current UTC day",
            Self::GrowthLimit => "local usage store exceeds its size limit",
            Self::Io(_) | Self::Sql(_) | Self::UnsafeReadState => {
                "local usage store could not be read"
            }
        }
    }
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("store error: {0}")]
    Store(#[from] ctx_history_store::StoreError),
}

pub type Result<T> = std::result::Result<T, SearchError>;

use std::{fmt, path::Path};

use anyhow::Result;

use super::{
    model_contract::{
        SEMANTIC_DIMENSIONS, SEMANTIC_MODEL_CONTRACT_VERSION, SEMANTIC_MODEL_ID,
        SEMANTIC_MODEL_REVISION, SEMANTIC_NORMALIZATION, SEMANTIC_POOLING,
        SEMANTIC_REQUIRED_MODEL_FILES,
    },
    vector_store::{
        control,
        flat_segments::{FlatModelContract, FlatSegmentStore, FlatStoreError},
        SemanticVectorStore,
    },
};

pub(super) const SEMANTIC_VECTOR_SCHEMA_VERSION: i64 = 2;
pub(super) const SEMANTIC_VECTOR_BACKEND_FLAT_F32: &str = "flat-f32";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticVectorFailureKind {
    Unavailable,
    StorageConflict,
    ResetRequired,
    NewerSchema,
}

#[derive(Debug)]
pub(super) struct SemanticVectorStoreError {
    pub(super) kind: SemanticVectorFailureKind,
    message: String,
}

impl SemanticVectorStoreError {
    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: SemanticVectorFailureKind::Unavailable,
            message: message.into(),
        }
    }

    pub(super) fn storage_conflict(message: impl Into<String>) -> Self {
        Self {
            kind: SemanticVectorFailureKind::StorageConflict,
            message: message.into(),
        }
    }

    pub(super) fn reset_required(message: impl Into<String>) -> Self {
        Self {
            kind: SemanticVectorFailureKind::ResetRequired,
            message: message.into(),
        }
    }

    pub(super) fn newer_schema(found: i64) -> Self {
        Self {
            kind: SemanticVectorFailureKind::NewerSchema,
            message: format!(
                "semantic vector store schema version {found} is newer than supported version {SEMANTIC_VECTOR_SCHEMA_VERSION}"
            ),
        }
    }
}

impl fmt::Display for SemanticVectorStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SemanticVectorStoreError {}

pub(super) fn semantic_vector_failure_kind(
    error: &anyhow::Error,
) -> Option<SemanticVectorFailureKind> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<SemanticVectorStoreError>()
            .map(|error| error.kind)
    })
}

impl SemanticVectorStore {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let flat = FlatSegmentStore::open(path, active_model_contract())
            .map_err(semantic_flat_store_error)?;
        let conn = control::open_writable(path)?;
        Ok(Self { conn, flat })
    }

    pub(super) fn open_read_only(path: &Path) -> Result<Option<Self>> {
        let Some(conn) = control::open_read_only(path)? else {
            return Ok(None);
        };
        let flat = FlatSegmentStore::open_read_only(path, active_model_contract())
            .map_err(semantic_flat_store_error)?;
        Ok(Some(Self { conn, flat }))
    }
}

pub(super) fn active_model_contract() -> FlatModelContract {
    let tokenizer = SEMANTIC_REQUIRED_MODEL_FILES
        .iter()
        .find(|file| file.path == "tokenizer.json")
        .map(|file| format!("sha256:{}", file.sha256))
        .unwrap_or_else(|| "missing-tokenizer-contract".to_owned());
    FlatModelContract {
        contract_version: SEMANTIC_MODEL_CONTRACT_VERSION,
        model_id: SEMANTIC_MODEL_ID.to_owned(),
        model_revision: SEMANTIC_MODEL_REVISION.to_owned(),
        tokenizer,
        pooling: SEMANTIC_POOLING.to_owned(),
        dimensions: SEMANTIC_DIMENSIONS as u32,
        normalization: SEMANTIC_NORMALIZATION.to_owned(),
    }
}

pub(super) fn semantic_owned_sidecar_result<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| match semantic_sqlite_error_code(&error) {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            SemanticVectorStoreError::reset_required(format!(
                "semantic control metadata operation failed; rebuild required: {error:#}"
            ))
            .into()
        }
        _ => error,
    })
}

pub(super) fn semantic_sqlite_error_code(error: &anyhow::Error) -> Option<rusqlite::ErrorCode> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(sqlite_error_code)
    })
}

fn sqlite_error_code(error: &rusqlite::Error) -> Option<rusqlite::ErrorCode> {
    let rusqlite::Error::SqliteFailure(failure, _) = error else {
        return None;
    };
    Some(failure.code)
}

fn semantic_flat_store_error(error: FlatStoreError) -> anyhow::Error {
    match &error {
        FlatStoreError::Corrupt(message) => SemanticVectorStoreError::reset_required(format!(
            "semantic flat F32 generation is corrupt; rebuild required: {message}"
        ))
        .into(),
        FlatStoreError::Incompatible(message) => SemanticVectorStoreError::reset_required(format!(
            "semantic flat F32 generation is incompatible; rebuild required: {message}"
        ))
        .into(),
        FlatStoreError::InvalidInput(message) => SemanticVectorStoreError::unavailable(format!(
            "invalid semantic flat F32 input: {message}"
        ))
        .into(),
        FlatStoreError::ReadOnly
        | FlatStoreError::Unsupported(_)
        | FlatStoreError::Io { .. }
        | FlatStoreError::Serialize(_) => anyhow::Error::new(error),
    }
}

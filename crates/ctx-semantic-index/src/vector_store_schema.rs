use std::{fmt, path::Path};

use anyhow::Result;
use ctx_semantic_model::SemanticModelContract;

use super::{
    private_fs,
    vector_store::{
        control,
        flat_segments::{
            FlatModelContract, FlatSegmentStore, FlatStoreCoordinationGuard, FlatStoreError,
        },
        SemanticVectorStore, SourceBackedGenerationPin,
    },
};

pub(super) const SEMANTIC_VECTOR_SCHEMA_VERSION: i64 = 6;
pub(super) const SEMANTIC_VECTOR_BACKEND_FLAT_F32: &str = "flat-f32";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticVectorFailureKind {
    Unavailable,
    PassiveSnapshotUnavailable,
    StorageConflict,
    ResetRequired,
    NewerSchema,
}

#[derive(Debug)]
pub struct SemanticVectorStoreError {
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

    pub(super) fn passive_snapshot_unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: SemanticVectorFailureKind::PassiveSnapshotUnavailable,
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

pub fn semantic_vector_failure_kind(error: &anyhow::Error) -> Option<SemanticVectorFailureKind> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<SemanticVectorStoreError>()
            .map(|error| error.kind)
    })
}

impl SemanticVectorStore {
    pub fn open(path: &Path, contract: &SemanticModelContract) -> Result<Self> {
        Self::open_with_private_root_ready(path, contract, || {})
    }

    fn open_with_private_root_ready(
        path: &Path,
        contract: &SemanticModelContract,
        private_root_ready: impl FnOnce(),
    ) -> Result<Self> {
        // Reject newer or incompatible control state before Flat recovery can
        // remove artifacts. The same validation runs again under coordination
        // in `open_writable`, closing the concurrent-writer window.
        control::preflight_writable_compatibility(path)?;
        // Establish the private semantic root before Flat can create any
        // directory or coordination artifact. Later Flat/control validation
        // remains authoritative for redirects and incompatible state.
        private_fs::create_private_dir_all(path)?;
        private_root_ready();
        let flat_contract = flat_model_contract(contract).map_err(semantic_flat_store_error)?;
        let (flat, coordination) = FlatSegmentStore::prepare_writable_open(path, flat_contract)
            .map_err(semantic_flat_store_error)?;
        // Recovery, control migration/open, and model-reset handoff share the
        // same transaction lock used by source reconciliation.
        let conn = control::open_writable(path)?;
        let flat = flat
            .finish_writable_open()
            .map_err(semantic_flat_store_error)?;
        let store = Self {
            conn,
            flat,
            contract: contract.clone(),
            _passive_snapshot: None,
        };
        if store
            .flat
            .model_contract_reset_pending()
            .map_err(semantic_flat_store_error)?
        {
            store.record_flat_model_contract_reset()?;
            store
                .flat
                .acknowledge_model_contract_reset()
                .map_err(semantic_flat_store_error)?;
        }
        drop(coordination);
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn open_after_private_root_ready(
        path: &Path,
        contract: &SemanticModelContract,
        private_root_ready: impl FnOnce(),
    ) -> Result<Self> {
        Self::open_with_private_root_ready(path, contract, private_root_ready)
    }

    pub fn open_read_only(path: &Path, contract: &SemanticModelContract) -> Result<Option<Self>> {
        let flat_contract = flat_model_contract(contract).map_err(semantic_flat_store_error)?;
        let Some(conn) = control::open_read_only(path)? else {
            return Ok(None);
        };
        let flat = FlatSegmentStore::open_read_only(path, flat_contract)
            .map_err(semantic_flat_store_error)?;
        if flat
            .model_contract_reset_pending()
            .map_err(semantic_flat_store_error)?
        {
            return Ok(None);
        }
        Ok(Some(Self {
            conn,
            flat,
            contract: contract.clone(),
            _passive_snapshot: None,
        }))
    }

    /// Opens a completed semantic snapshot without giving SQLite any write
    /// capability. This is the only store opener suitable for daemon-free
    /// passive queries.
    pub(crate) fn open_passive_snapshot(
        path: &Path,
        contract: &SemanticModelContract,
    ) -> Result<Option<Self>> {
        Self::open_passive_snapshot_with_admission(path, contract, || {})
    }

    fn open_passive_snapshot_with_admission(
        path: &Path,
        contract: &SemanticModelContract,
        admitted: impl FnOnce(),
    ) -> Result<Option<Self>> {
        let Some(path) = control::passive_snapshot_root(path)? else {
            return Ok(None);
        };
        let passive_snapshot = FlatStoreCoordinationGuard::lock_passive_snapshot(&path)
            .map_err(passive_flat_store_error)?;
        admitted();
        passive_snapshot
            .validate_retained()
            .map_err(passive_flat_store_error)?;
        let Some(conn) = control::open_passive_snapshot(&path)? else {
            return Ok(None);
        };
        let flat_contract = flat_model_contract(contract).map_err(semantic_flat_store_error)?;
        let flat = FlatSegmentStore::open_read_only(&path, flat_contract)
            .map_err(semantic_flat_store_error)?;
        if flat
            .model_contract_reset_pending()
            .map_err(semantic_flat_store_error)?
        {
            return Ok(None);
        }
        passive_snapshot
            .validate_retained()
            .map_err(passive_flat_store_error)?;
        Ok(Some(Self {
            conn,
            flat,
            contract: contract.clone(),
            _passive_snapshot: Some(passive_snapshot),
        }))
    }

    #[cfg(test)]
    pub(crate) fn open_passive_snapshot_after_admission(
        path: &Path,
        contract: &SemanticModelContract,
        admitted: impl FnOnce(),
    ) -> Result<Option<Self>> {
        Self::open_passive_snapshot_with_admission(path, contract, admitted)
    }

    pub(super) fn validate_passive_snapshot_coordination(&self) -> Result<()> {
        if let Some(snapshot) = &self._passive_snapshot {
            snapshot
                .validate_retained()
                .map_err(passive_flat_store_error)?;
        }
        Ok(())
    }

    /// Pins one exact passive generation while the writer coordination lock is
    /// held, then releases the lock before returning the immutable flat pin.
    pub fn source_backed_generation_pin_passive(
        path: &Path,
        contract: &SemanticModelContract,
        core_generation_id: &str,
        semantic_documents: u64,
    ) -> Result<Option<SourceBackedGenerationPin>> {
        let Some(store) = Self::open_passive_snapshot(path, contract)? else {
            return Ok(None);
        };
        store
            .source_backed_generation_pin_exact(core_generation_id, semantic_documents)
            .map(Some)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn commit_control_wal_for_test(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE semantic_index_stats SET dirty_items = dirty_items",
            [],
        )?;
        Ok(())
    }
}

pub(super) fn flat_model_contract(
    contract: &SemanticModelContract,
) -> std::result::Result<FlatModelContract, FlatStoreError> {
    let dimensions = u32::try_from(contract.dimensions()).map_err(|_| {
        FlatStoreError::InvalidInput(format!(
            "model contract dimensions {} exceed the flat F32 format",
            contract.dimensions()
        ))
    })?;
    let flat = FlatModelContract {
        contract_version: contract.contract_version(),
        model_id: contract.model_id().to_owned(),
        model_revision: contract.model_revision().to_owned(),
        tokenizer: contract.tokenizer_fingerprint().to_owned(),
        pooling: contract.pooling().to_owned(),
        dimensions,
        normalization: contract.normalization().to_owned(),
    };
    flat.validate()?;
    Ok(flat)
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
        FlatStoreError::LegacySchema(version) => SemanticVectorStoreError::reset_required(format!(
            "semantic flat F32 generation uses legacy schema {version}; rebuild required"
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

fn passive_flat_store_error(error: FlatStoreError) -> anyhow::Error {
    SemanticVectorStoreError::passive_snapshot_unavailable(format!(
        "passive semantic snapshot coordination is unavailable: {error}"
    ))
    .into()
}

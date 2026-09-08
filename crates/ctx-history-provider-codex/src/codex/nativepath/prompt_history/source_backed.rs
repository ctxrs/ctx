//! Shared-family projection for Codex's ordinary `history.jsonl` prompt log.

use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CoreRecord, CoreRecordError, ProjectionContractError, SourceAnchor, SourceKey,
};
use thiserror::Error;

use super::super::absolute_lexical_path;
use super::PromptLine;
use crate::provider::source_backed::ProviderRuntimeBinding;
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::family::jsonl::{
        jsonl_single_file_inventory, JsonlFamilyAdapter, JsonlFamilyAppendMode,
        JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf, JsonlFamilyProjector,
        JsonlFamilyRootMissingMode, JsonlFamilyWorkerContext, JsonlOversizedRecordPolicy,
        JsonlRecordRef,
    },
    CaptureError,
};

mod projection;
use projection::{core_record, retained_record_bytes};

pub(crate) const SOURCE_FORMAT: &str = "codex_history_jsonl";
const PATH_KIND: &str = "Codex prompt-history JSONL";
const SOURCE_SCHEMA_VARIANT: &str = "codex-prompt-history-jsonl-v1";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const PARSER_REVISION: &str = "codex-prompt-history-shared-jsonl-v5";
const SESSION_KEY_NAMESPACE: &str = "codex.prompt-history.session";
const EVENT_POSITION_KIND: &str = "codex.prompt-history.raw-ordinal";
const LOGICAL_SESSION_KIND: &str = "codex-prompt-history-session";
const LOGICAL_EVENT_KIND: &str = "codex-prompt-history-event";
const MAX_RETAINED_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum CodexPromptHistorySourceBackedErrorV0 {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error("Codex prompt-history Core record exceeds its retained-record bound")]
    RecordTooLarge,
}

pub type CodexPromptHistorySourceBackedResultV0<T> =
    Result<T, CodexPromptHistorySourceBackedErrorV0>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexPromptHistorySourceBackedInputV0 {
    path: PathBuf,
    catalog_lineage: [u8; 32],
}

impl CodexPromptHistorySourceBackedInputV0 {
    pub fn explicit(path: impl Into<PathBuf>, catalog_lineage: [u8; 32]) -> Self {
        Self {
            path: path.into(),
            catalog_lineage,
        }
    }

    pub fn source_key(&self) -> CodexPromptHistorySourceBackedResultV0<SourceKey> {
        Ok(SourceKey::derive(
            CaptureProvider::Codex.as_str(),
            SOURCE_FORMAT,
            SOURCE_SCHEMA_VARIANT,
            SOURCE_IDENTITY_VERSION,
            SourceAnchor::CatalogLineage(self.catalog_lineage),
        )?)
    }
}

/// The shared family owns framing, checkpoints, append classification, paging,
/// publication, deletion, and terminal validation. This adapter supplies only
/// the exact source binding and per-record Codex projection.
#[derive(Clone)]
pub struct CodexPromptHistoryJsonlFamilyAdapterV0<B: ProviderRuntimeBinding> {
    route_path: Box<Path>,
    source: SourceKey,
    binding: PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> CodexPromptHistoryJsonlFamilyAdapterV0<B> {
    pub fn new(
        input: CodexPromptHistorySourceBackedInputV0,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        Ok(Self {
            route_path: absolute_lexical_path(&input.path)?.into_boxed_path(),
            source: input.source_key()?,
            binding: PhantomData,
        })
    }

    pub fn route_path(&self) -> &Path {
        &self.route_path
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilyAdapter for CodexPromptHistoryJsonlFamilyAdapterV0<B> {
    type Runtime = crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime<B>;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &'static str {
        SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
    }

    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::AuthoritativeEmpty
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        if root != self.route_path() {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history JSONL route path changed".to_owned(),
            ));
        }
        jsonl_single_file_inventory(self.provider(), root, self.source.clone(), PATH_KIND)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<
        Box<
            dyn JsonlFamilyProjector<
                Runtime = crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime<B>,
            >,
        >,
    > {
        if !self.source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Box::new(CodexPromptHistoryProjector {
            source: self.source.clone(),
            rejected_records: 0,
            binding: PhantomData,
        }))
    }
}

pub struct CodexPromptHistoryProjector<B: ProviderRuntimeBinding> {
    source: SourceKey,
    rejected_records: u64,
    pub binding: PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> CodexPromptHistoryProjector<B> {
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        input: CodexPromptHistorySourceBackedInputV0,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        Ok(Self {
            source: input.source_key()?,
            rejected_records: 0,
            binding: PhantomData,
        })
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilyProjector for CodexPromptHistoryProjector<B> {
    type Runtime = crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime<B>;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<B>,
        emit: &mut dyn FnMut(CoreRecord) -> crate::Result<()>,
    ) -> crate::Result<()> {
        project_prompt_record(&self.source, &mut self.rejected_records, record, emit)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }
}

fn project_prompt_record(
    source: &SourceKey,
    rejected_records: &mut u64,
    record: JsonlRecordRef<'_>,
    emit: &mut dyn FnMut(CoreRecord) -> crate::Result<()>,
) -> crate::Result<()> {
    if record.oversized() {
        *rejected_records = rejected_records.saturating_add(1);
        return Ok(());
    }
    if record.bytes().iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    let line = match serde_json::from_slice::<PromptLine>(record.bytes()) {
        Ok(line)
            if !line.session_id.trim().is_empty()
                && chrono::DateTime::from_timestamp(line.ts, 0).is_some() =>
        {
            line
        }
        _ => {
            *rejected_records = rejected_records.saturating_add(1);
            return Ok(());
        }
    };
    let projected = core_record(source, line, record.evidence().physical_ordinal())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if retained_record_bytes(&projected) > MAX_RETAINED_RECORD_BYTES {
        // A bounded record rejection must not discard valid neighboring prompts.
        *rejected_records = rejected_records.saturating_add(1);
        return Ok(());
    }
    emit(projected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_record_limit_rejects_one_prompt_and_preserves_valid_peers() {
        let source =
            CodexPromptHistorySourceBackedInputV0::explicit("/tmp/codex/history.jsonl", [0x42; 32])
                .source_key()
                .unwrap();
        let oversized = serde_json::json!({"session_id":"oversized", "ts":2,
            "text":"x".repeat(MAX_RETAINED_RECORD_BYTES)})
        .to_string();
        let rows = [
            br#"{"session_id":"before","ts":1,"text":"before"}"#.as_slice(),
            oversized.as_bytes(),
            br#"{"session_id":"after","ts":3,"text":"after"}"#.as_slice(),
        ];
        let mut rejected = 0;
        let mut projected = Vec::new();
        for (ordinal, row) in rows.into_iter().enumerate() {
            project_prompt_record(
                &source,
                &mut rejected,
                JsonlRecordRef::for_test(row, ordinal as u64),
                &mut |record| {
                    projected.push(record);
                    Ok(())
                },
            )
            .unwrap();
        }
        assert_eq!(
            projected
                .iter()
                .map(|r| r.content.meaningful_text())
                .collect::<Vec<_>>(),
            ["before", "after"]
        );
        assert_eq!(rejected, 1);
    }

    #[test]
    fn named_root_lineage_remains_the_root_singleton_catalog_anchor() {
        let root_lineage = [0x5a; 32];
        let source = CodexPromptHistorySourceBackedInputV0::explicit(
            "/old/codex/history.jsonl",
            root_lineage,
        )
        .source_key()
        .unwrap();
        let moved = CodexPromptHistorySourceBackedInputV0::explicit(
            "/new/codex/history.jsonl",
            root_lineage,
        )
        .source_key()
        .unwrap();
        let released = SourceKey::derive(
            CaptureProvider::Codex.as_str(),
            SOURCE_FORMAT,
            SOURCE_SCHEMA_VARIANT,
            SOURCE_IDENTITY_VERSION,
            SourceAnchor::CatalogLineage(root_lineage),
        )
        .unwrap();

        assert_eq!(source.anchor(), &SourceAnchor::CatalogLineage(root_lineage));
        assert!(source.exact_descriptor_eq(&moved));
        assert_eq!(
            source.identity().encode_canonical().unwrap(),
            released.identity().encode_canonical().unwrap()
        );
    }
}

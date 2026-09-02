//! Shared-family projection for Codex's ordinary `history.jsonl` prompt log.

use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::SourceBackedRecordRejectionDrafts;
use ctx_history_core::{
    CaptureProvider, CoreRecord, CoreRecordError, ProjectionContractError, SourceAnchor, SourceKey,
};
use ctx_history_jsonl::JsonlRecordRejections;
use thiserror::Error;

use super::super::absolute_lexical_path;
use super::PromptLine;
use crate::provider::source_backed::ProviderRuntimeBinding;
use crate::MAX_PROVIDER_JSONL_LINE_BYTES;
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
const PARSER_REVISION: &str = "codex-prompt-history-shared-jsonl-v4";
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
            rejections: JsonlRecordRejections::new(
                self.source.clone(),
                CaptureProvider::Codex,
                leaf.source_path().display().to_string(),
            ),
            binding: PhantomData,
        }))
    }
}

pub struct CodexPromptHistoryProjector<B: ProviderRuntimeBinding> {
    source: SourceKey,
    rejections: JsonlRecordRejections,
    pub binding: PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> CodexPromptHistoryProjector<B> {
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        input: CodexPromptHistorySourceBackedInputV0,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        let source = input.source_key()?;
        Ok(Self {
            source: source.clone(),
            rejections: JsonlRecordRejections::new(
                source,
                CaptureProvider::Codex,
                input.path.display().to_string(),
            ),
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
        project_prompt_record(&self.source, &mut self.rejections, record, emit)
    }

    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
    }
}

fn project_prompt_record(
    source: &SourceKey,
    rejections: &mut JsonlRecordRejections,
    record: JsonlRecordRef<'_>,
    emit: &mut dyn FnMut(CoreRecord) -> crate::Result<()>,
) -> crate::Result<()> {
    if record.oversized() {
        rejections.malformed(
            record,
            format!(
                "Codex prompt-history record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
            ),
        );
        return Ok(());
    }
    if record.bytes().iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    let line = match serde_json::from_slice::<PromptLine>(record.bytes()) {
        Ok(line) if line.session_id.trim().is_empty() => {
            rejections.malformed(record, "Codex prompt-history session_id is empty");
            return Ok(());
        }
        Ok(line) if chrono::DateTime::from_timestamp(line.ts, 0).is_none() => {
            rejections.malformed(record, "Codex prompt-history timestamp is out of range");
            return Ok(());
        }
        Ok(line) => line,
        Err(error) => {
            rejections.malformed(
                record,
                format!("malformed Codex prompt-history JSONL: {error}"),
            );
            return Ok(());
        }
    };
    let projected = core_record(source, line, record.evidence().physical_ordinal())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if retained_record_bytes(&projected) > MAX_RETAINED_RECORD_BYTES {
        return Err(CaptureError::InvalidPayload(
            CodexPromptHistorySourceBackedErrorV0::RecordTooLarge.to_string(),
        ));
    }
    emit(projected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_capture_runtime::MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS;

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

    #[test]
    fn malformed_records_keep_valid_peers_and_report_exact_locations_and_reasons() {
        let input =
            CodexPromptHistorySourceBackedInputV0::explicit("/tmp/codex/history.jsonl", [0x2a; 32]);
        let source = input.source_key().unwrap();
        let mut rejections = JsonlRecordRejections::new(
            source.clone(),
            CaptureProvider::Codex,
            input.path.display().to_string(),
        );
        let rows: [&[u8]; 5] = [
            br#"{"session_id":"before","ts":1,"text":"before"}"#,
            br#"{"#,
            br#"[]"#,
            br#"{"session_id":" ","ts":2,"text":"empty identity"}"#,
            br#"{"session_id":"after","ts":3,"text":"after"}"#,
        ];
        let mut projected = Vec::new();
        for (ordinal, row) in rows.into_iter().enumerate() {
            project_prompt_record(
                &source,
                &mut rejections,
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
                .map(|record| record.content.meaningful_text())
                .collect::<Vec<_>>(),
            ["before", "after"]
        );
        assert_eq!(rejections.count(), 3);
        let (drafts, omitted) = rejections.take_drafts().into_parts();
        assert_eq!(omitted, 0);
        assert_eq!(
            drafts
                .iter()
                .map(|draft| draft.line_number)
                .collect::<Vec<_>>(),
            [2, 3, 4]
        );
        assert!(drafts[0]
            .detail
            .contains("malformed Codex prompt-history JSONL"));
        assert!(drafts[1]
            .detail
            .contains("malformed Codex prompt-history JSONL"));
        assert_eq!(drafts[2].detail, "Codex prompt-history session_id is empty");
    }

    #[test]
    fn all_invalid_records_keep_an_exact_count_with_bounded_details() {
        let source = CodexPromptHistorySourceBackedInputV0::explicit(
            "/tmp/codex/all-invalid.jsonl",
            [0x31; 32],
        )
        .source_key()
        .unwrap();
        let mut rejections = JsonlRecordRejections::new(
            source.clone(),
            CaptureProvider::Codex,
            "/tmp/codex/all-invalid.jsonl",
        );
        let rejected = MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS + 1;
        let mut projected = Vec::new();
        for ordinal in 0..rejected {
            project_prompt_record(
                &source,
                &mut rejections,
                JsonlRecordRef::for_test(b"{", ordinal as u64),
                &mut |record| {
                    projected.push(record);
                    Ok(())
                },
            )
            .unwrap();
        }

        assert!(projected.is_empty());
        assert_eq!(rejections.count(), rejected as u64);
        let (drafts, omitted) = rejections.take_drafts().into_parts();
        assert_eq!(drafts.len(), MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS);
        assert_eq!(omitted, 1);
        assert_eq!(drafts.len() + omitted, rejected);
    }
}

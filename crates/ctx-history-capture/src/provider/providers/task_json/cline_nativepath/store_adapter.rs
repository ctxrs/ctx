use std::collections::BTreeMap;

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    provider::native_ingestion::{
        NativeIngestionPage, NativeIngestionPageError, NativePageAccounting, NativeProOutputPage,
        NativeProReplayPage, NativePublicationPage, NativeSafeFrontier, NativeSourceIdentity,
    },
    ImportProfile, OutputNativeCursor, OutputSourceIdentity, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition,
};

use super::{
    normalize::{estimated_output_bytes, estimated_rejection_bytes},
    ClineCertifiedPage, ClinePageFrontier, ClineTaskIdentityOrigin, ClineTransientOutputPayload,
};

const CLINE_NATIVE_FRONTIER_VERSION: u32 = 1;
const TASK_JSON_OUTPUT_PARSER_REVISION: &str = "task-json-nativepath-v1";

#[derive(Debug, Error)]
pub(super) enum ClineNativePageAdapterError {
    #[error(transparent)]
    Page(#[from] NativeIngestionPageError),
    #[error(transparent)]
    Output(#[from] ProOutputSinkError),
    #[error("Cline NativePath output progress is not a certified Cline frontier")]
    InvalidOutputFrontier,
    #[error("Cline NativePath output epoch is exhausted")]
    OutputEpochExhausted,
    #[error("Cline NativePath page provider does not match the selected production provider")]
    ProviderMismatch,
}

#[derive(Debug)]
pub(super) struct ClineAdaptedPage {
    pub(crate) core: NativePublicationPage<ClineCertifiedPage>,
    pub(crate) output: Option<ClinePendingOutput>,
}

#[derive(Debug)]
pub(super) struct ClinePendingOutput {
    source_key: Box<str>,
    source_identity: NativeSourceIdentity,
    observed_revision: String,
    expected_frontier: NativeSafeFrontier,
    next_safe_frontier: NativeSafeFrontier,
    terminal: bool,
    transient: Option<ClineTransientOutputPayload>,
}

#[derive(Debug)]
struct OutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    observed_revision: String,
}

/// Provider-local adaptation from certified task-JSON component pages into the
/// shared bounded mechanics. Canonical Store row construction remains in the
/// family publication consumer.
pub(super) struct ClineNativePageAdapter<'a> {
    provider: CaptureProvider,
    sink: Option<&'a dyn ProOutputSink>,
    output_by_source: BTreeMap<Box<str>, OutputState>,
}

impl<'a> ClineNativePageAdapter<'a> {
    pub(super) fn new(provider: CaptureProvider, profile: &'a ImportProfile) -> Self {
        Self {
            provider,
            sink: profile.sink().map(std::sync::Arc::as_ref),
            output_by_source: BTreeMap::new(),
        }
    }

    pub(super) fn adapt(
        &mut self,
        mut page: ClineCertifiedPage,
    ) -> Result<ClineAdaptedPage, ClineNativePageAdapterError> {
        if page.source.provider != self.provider.as_str() {
            return Err(ClineNativePageAdapterError::ProviderMismatch);
        }
        let source_key = page.source.stable_id.clone();
        let source_identity =
            NativeSourceIdentity::new(self.provider.as_str(), source_key.to_string());
        let expected_frontier = safe_frontier(&page.expected_frontier)?;
        let next_safe_frontier = safe_frontier(&page.next_safe_frontier)?;
        let source_revision = revision(&page.source_revision.revision_sha256);
        let transient = page.transient.take();
        let core_accounting = NativePageAccounting {
            logical_units: page.accounting.core_units,
            conservative_serialized_bytes: page.accounting.conservative_core_bytes,
        };
        let core = NativeIngestionPage::new(
            expected_frontier.clone(),
            next_safe_frontier.clone(),
            page.terminal,
            core_accounting,
            page,
        )?;
        let output = self.sink.map(|_| ClinePendingOutput {
            source_key,
            source_identity: source_identity.clone(),
            observed_revision: source_revision,
            expected_frontier,
            next_safe_frontier,
            terminal: core.terminal,
            transient,
        });
        Ok(ClineAdaptedPage {
            core: NativePublicationPage::new(source_identity, core),
            output,
        })
    }

    /// This is intentionally separate from `adapt`: callers must publish or
    /// verify Core before this method is allowed to observe Pro state.
    pub(super) fn adapt_output_after_core(
        &mut self,
        pending: ClinePendingOutput,
    ) -> Result<Option<NativeProReplayPage>, ClineNativePageAdapterError> {
        self.adapt_output(
            &pending.source_key,
            pending.source_identity,
            pending.observed_revision,
            pending.expected_frontier,
            pending.next_safe_frontier,
            pending.terminal,
            pending.transient,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn adapt_output(
        &mut self,
        source_key: &str,
        source_identity: NativeSourceIdentity,
        observed_revision: String,
        expected_frontier: NativeSafeFrontier,
        next_safe_frontier: NativeSafeFrontier,
        terminal: bool,
        transient: Option<ClineTransientOutputPayload>,
    ) -> Result<Option<NativeProReplayPage>, ClineNativePageAdapterError> {
        let Some(sink) = self.sink else {
            return Ok(None);
        };
        let transient = transient.unwrap_or(ClineTransientOutputPayload {
            observations: Vec::new(),
            rejected_outputs: Box::new([]),
        });
        let state = match self.output_by_source.get_mut(source_key) {
            Some(state) => state,
            None => {
                let source = OutputSourceIdentity {
                    provider: self.provider.as_str().to_owned(),
                    namespace_id: source_key.to_owned(),
                    source_id: source_key.to_owned(),
                };
                let progress = sink.observe_source(&source)?;
                let state = initial_output_state(
                    source,
                    progress,
                    &observed_revision,
                    &expected_frontier,
                    sink.materializer_revision(),
                    self.provider,
                )?;
                self.output_by_source
                    .insert(source_key.to_owned().into_boxed_str(), state);
                self.output_by_source
                    .get_mut(source_key)
                    .ok_or(ClineNativePageAdapterError::InvalidOutputFrontier)?
            }
        };
        if state.observed_revision != observed_revision
            && state.disposition == ProOutputSourceDisposition::AppendOrResume
        {
            state.source_epoch = state
                .source_epoch
                .checked_add(1)
                .ok_or(ClineNativePageAdapterError::OutputEpochExhausted)?;
            state.expected_source_epoch = state.source_epoch.checked_sub(1);
            state.disposition = ProOutputSourceDisposition::Rewrite;
            state.observed_revision = observed_revision.clone();
        }
        let observations = transient.observations;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision,
            parser_revision: output_parser_revision(self.provider),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_sink_frontier.clone(),
            observations,
        };
        let accounting = NativePageAccounting {
            logical_units: output.observations.len(),
            conservative_serialized_bytes: estimated_replay_page_bytes(
                &source_identity,
                &expected_frontier,
                &next_safe_frontier,
                &output,
            )
            .saturating_add(
                transient
                    .rejected_outputs
                    .iter()
                    .map(estimated_rejection_bytes)
                    .sum::<usize>(),
            ),
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            source_identity,
            expected_frontier,
            next_safe_frontier.clone(),
            terminal,
            accounting,
            output,
        )?;
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_sink_frontier = Some(next_safe_frontier);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
        Ok(Some(replay))
    }
}

fn initial_output_state(
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    observed_revision: &str,
    expected_frontier: &NativeSafeFrontier,
    materializer_revision: &str,
    provider: CaptureProvider,
) -> Result<OutputState, ClineNativePageAdapterError> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
            observed_revision: observed_revision.to_owned(),
        });
    };
    let prior = progress
        .cursor
        .as_ref()
        .map(output_safe_frontier)
        .transpose()?;
    let rewrite = progress.parser_revision != output_parser_revision(provider)
        || progress.materializer_revision != materializer_revision
        || progress.observed_revision != observed_revision
        || prior
            .as_ref()
            .is_some_and(|prior| prior != expected_frontier);
    Ok(OutputState {
        source,
        source_epoch: if rewrite {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(ClineNativePageAdapterError::OutputEpochExhausted)?
        } else {
            progress.source_epoch
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier: prior,
        disposition: if rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            ProOutputSourceDisposition::AppendOrResume
        },
        observed_revision: observed_revision.to_owned(),
    })
}

fn output_parser_revision(provider: CaptureProvider) -> String {
    format!("{}:{TASK_JSON_OUTPUT_PARSER_REVISION}", provider.as_str())
}

fn output_safe_frontier(
    cursor: &OutputNativeCursor,
) -> Result<NativeSafeFrontier, ClineNativePageAdapterError> {
    if cursor.version != CLINE_NATIVE_FRONTIER_VERSION {
        return Err(ClineNativePageAdapterError::InvalidOutputFrontier);
    }
    serde_json::from_slice::<ClinePageFrontier>(&cursor.payload)
        .map_err(|_| ClineNativePageAdapterError::InvalidOutputFrontier)?;
    NativeSafeFrontier::new(cursor.version, cursor.payload.clone())
        .map_err(ClineNativePageAdapterError::from)
}

fn safe_frontier(
    frontier: &ClinePageFrontier,
) -> Result<NativeSafeFrontier, ClineNativePageAdapterError> {
    let bytes = serde_json::to_vec(frontier)
        .map_err(|_| ClineNativePageAdapterError::InvalidOutputFrontier)?;
    NativeSafeFrontier::new(CLINE_NATIVE_FRONTIER_VERSION, bytes)
        .map_err(ClineNativePageAdapterError::from)
}

fn estimated_replay_page_bytes(
    source: &NativeSourceIdentity,
    expected: &NativeSafeFrontier,
    next: &NativeSafeFrontier,
    output: &NativeProOutputPage,
) -> usize {
    32_usize
        .saturating_add(encoded_str(source.provider()))
        .saturating_add(encoded_str(source.source_identity()))
        .saturating_add(encoded_frontier(expected))
        .saturating_add(encoded_frontier(next))
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(encoded_str(&output.source.provider))
        .saturating_add(encoded_str(&output.source.namespace_id))
        .saturating_add(encoded_str(&output.source.source_id))
        .saturating_add(8)
        .saturating_add(encoded_str(&output.observed_revision))
        .saturating_add(encoded_str(&output.parser_revision))
        .saturating_add(encoded_str(&output.materializer_revision))
        .saturating_add(1)
        .saturating_add(1 + usize::from(output.expected_prior_source_epoch.is_some()) * 8)
        .saturating_add(encoded_optional_frontier(
            output.expected_prior_frontier.as_ref(),
        ))
        .saturating_add(8)
        .saturating_add(
            output
                .observations
                .iter()
                .map(estimated_output_bytes)
                .sum::<usize>(),
        )
}

fn encoded_frontier(frontier: &NativeSafeFrontier) -> usize {
    4_usize.saturating_add(encoded_bytes(&frontier.bytes))
}

fn encoded_optional_frontier(frontier: Option<&NativeSafeFrontier>) -> usize {
    1_usize.saturating_add(frontier.map_or(0, encoded_frontier))
}

fn encoded_str(value: &str) -> usize {
    encoded_bytes(value.as_bytes())
}

fn encoded_bytes(value: &[u8]) -> usize {
    8_usize.saturating_add(value.len())
}

fn revision(hash: &[u8; 32]) -> String {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClineNativeStoreCursor {
    pub(crate) version: u32,
    pub(crate) provider: String,
    pub(crate) source_identity: String,
    pub(crate) source_revision: String,
    pub(crate) frontier: ClinePageFrontier,
    pub(crate) terminal: bool,
    pub(crate) generation: u64,
    pub(crate) rejected_records: u64,
    #[serde(default)]
    pub(crate) task_identity: Option<String>,
    #[serde(default)]
    pub(crate) task_identity_origin: Option<u8>,
    #[serde(default)]
    pub(crate) task_identity_aliases: Vec<String>,
}

impl ClineNativeStoreCursor {
    pub(crate) const VERSION: u32 = 2;
    pub(crate) const LEGACY_VERSION: u32 = 1;

    pub(crate) fn task_origin(&self) -> Option<ClineTaskIdentityOrigin> {
        match self.task_identity_origin {
            Some(0) => Some(ClineTaskIdentityOrigin::TaskMetadata),
            Some(1) => Some(ClineTaskIdentityOrigin::DirectoryNameDegraded),
            _ => None,
        }
    }

    pub(crate) fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_store_cursor_round_trips() {
        let cursor = ClineNativeStoreCursor {
            version: ClineNativeStoreCursor::VERSION,
            provider: "cline".to_owned(),
            source_identity: "source".to_owned(),
            source_revision: "revision".to_owned(),
            frontier: ClinePageFrontier {
                version: 1,
                next_native_index: 7,
                prefix_semantic_sha256: [9; 32],
            },
            terminal: true,
            generation: 3,
            rejected_records: 2,
            task_identity: Some("task".to_owned()),
            task_identity_origin: Some(0),
            task_identity_aliases: vec!["old-task".to_owned()],
        };
        let encoded = cursor.encode().expect("encode");
        assert_eq!(
            ClineNativeStoreCursor::decode(&encoded).expect("decode"),
            cursor
        );
    }

    #[test]
    fn frontier_rejects_wrong_output_cursor_version() {
        let cursor = OutputNativeCursor {
            version: 2,
            payload: Vec::new(),
        };
        assert!(matches!(
            output_safe_frontier(&cursor),
            Err(ClineNativePageAdapterError::InvalidOutputFrontier)
        ));
    }

    #[test]
    fn legacy_v1_cursor_decodes_for_one_way_authority_upgrade() {
        let encoded = serde_json::json!({
            "version": 1,
            "provider": "roo-code",
            "source_identity": "source",
            "source_revision": "revision",
            "frontier": {
                "version": 1,
                "next_native_index": 0,
                "prefix_semantic_sha256": vec![0; 32],
            },
            "terminal": false,
            "generation": 0,
            "rejected_records": 0,
        })
        .to_string();
        let cursor = ClineNativeStoreCursor::decode(&encoded).expect("legacy cursor");
        assert_eq!(cursor.version, ClineNativeStoreCursor::LEGACY_VERSION);
        assert!(cursor.task_identity.is_none());
        assert!(cursor.task_identity_origin.is_none());
        assert!(cursor.task_identity_aliases.is_empty());
    }
}

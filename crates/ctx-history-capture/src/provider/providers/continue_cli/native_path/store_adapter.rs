use std::collections::BTreeMap;

use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::provider::native_ingestion::{
    NativeIngestionPage, NativeIngestionPageError, NativePageAccounting, NativeProOutputPage,
    NativeProReplayPage, NativePublicationPage, NativeSafeFrontier, NativeSourceIdentity,
};
use crate::{
    ImportProfile, OutputNativeCursor, OutputSourceIdentity, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition,
};

use super::{
    normalize::estimated_output_bytes, ContinueGenerationAuthority, ContinuePreparedPage,
    ContinuePreparedSource, ContinueSessionIdentity, ContinueTransientOutputPayload,
};

const CONTINUE_NATIVE_FRONTIER_VERSION: u32 = 1;
const CONTINUE_FRONTIER_HASH_DOMAIN: &[u8] = b"ctx-continue-nativepath-frontier-v1\0";
const CONTINUE_OUTPUT_PARSER_REVISION: &str = "continue-nativepath-v1";

#[derive(Debug, Error)]
pub(crate) enum ContinueNativePageAdapterError {
    #[error(transparent)]
    Page(#[from] NativeIngestionPageError),
    #[error("Continue NativePath page stream did not begin with source/session authority")]
    MissingSourceAuthority,
    #[error("Continue NativePath page belongs to a different active session")]
    SessionMismatch,
    #[error("Continue NativePath page ordinal/frontier chain is discontinuous")]
    FrontierMismatch,
    #[error("Continue NativePath terminal authority does not certify the observed history range")]
    InvalidTerminalAuthority,
    #[error("Continue NativePath frontier cannot be encoded")]
    FrontierEncoding,
    #[error("Continue NativePath output progress is not a certified Continue frontier")]
    InvalidOutputFrontier,
    #[error("Continue NativePath output epoch is exhausted")]
    OutputEpochExhausted,
}

#[derive(Debug)]
pub(crate) struct ContinueAdaptedPage {
    pub(crate) core: NativePublicationPage<ContinuePreparedPage>,
    pub(crate) output: Option<NativeProReplayPage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContinuePageFrontier {
    pub(crate) version: u32,
    pub(crate) next_page_ordinal: u64,
    pub(crate) next_history_ordinal: u64,
    pub(crate) prefix_semantic_sha256: [u8; 32],
}

impl ContinuePageFrontier {
    fn initial() -> Self {
        let prefix_semantic_sha256 = Sha256::digest(CONTINUE_FRONTIER_HASH_DOMAIN).into();
        Self {
            version: CONTINUE_NATIVE_FRONTIER_VERSION,
            next_page_ordinal: 0,
            next_history_ordinal: 0,
            prefix_semantic_sha256,
        }
    }
}

#[derive(Debug)]
struct ActiveSource {
    session: ContinueSessionIdentity,
    source_identity: NativeSourceIdentity,
    source_revision: String,
    frontier: ContinuePageFrontier,
}

#[derive(Debug)]
struct OutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    observed_revision: String,
    resume_through: Option<ContinuePageFrontier>,
}

/// Adapts Continue's provider-owned whole-document pages to the shared bounded
/// page contract. The adapter intentionally owns no Store mutation policy.
pub(crate) struct ContinueNativePageAdapter<'a> {
    active: Option<ActiveSource>,
    sink: Option<&'a dyn ProOutputSink>,
    output_by_source: BTreeMap<Box<str>, OutputState>,
}

impl<'a> ContinueNativePageAdapter<'a> {
    pub(crate) fn new(profile: &'a ImportProfile) -> Self {
        Self {
            active: None,
            sink: profile.sink().map(std::sync::Arc::as_ref),
            output_by_source: BTreeMap::new(),
        }
    }

    pub(crate) fn adapt(
        &mut self,
        mut page: ContinuePreparedPage,
    ) -> Result<ContinueAdaptedPage, ContinueNativePageAdapterError> {
        self.ensure_source(&page)?;
        let state = self
            .active
            .as_mut()
            .ok_or(ContinueNativePageAdapterError::MissingSourceAuthority)?;
        if state.session != page.session_identity {
            return Err(ContinueNativePageAdapterError::SessionMismatch);
        }
        if page.page_ordinal != state.frontier.next_page_ordinal {
            return Err(ContinueNativePageAdapterError::FrontierMismatch);
        }

        let expected = state.frontier.clone();
        let next_history_ordinal = next_history_ordinal(&page, &expected)?;
        let next = ContinuePageFrontier {
            version: CONTINUE_NATIVE_FRONTIER_VERSION,
            next_page_ordinal: expected
                .next_page_ordinal
                .checked_add(1)
                .ok_or(ContinueNativePageAdapterError::FrontierMismatch)?,
            next_history_ordinal,
            prefix_semantic_sha256: advance_prefix(&expected, &page),
        };
        let expected_frontier = safe_frontier(&expected)?;
        let next_safe_frontier = safe_frontier(&next)?;
        let source_identity = state.source_identity.clone();
        let source_revision = state.source_revision.clone();
        let session_identity = state.session.clone();
        let transient = page.transient_output.take();
        let accounting = NativePageAccounting {
            logical_units: page.row_count,
            conservative_serialized_bytes: page.estimated_bytes,
        };
        let terminal = page.terminal;
        let core = NativeIngestionPage::new(
            expected_frontier.clone(),
            next_safe_frontier.clone(),
            terminal,
            accounting,
            page,
        )?;
        state.frontier = next;
        let output = match self.adapt_output(
            &session_identity,
            source_identity.clone(),
            source_revision,
            expected_frontier,
            next_safe_frontier,
            terminal,
            transient,
        ) {
            Ok((output, _output_error)) => output,
            Err(error) => {
                let output_error =
                    ProOutputSinkError::new("continue_output_adapter", error.to_string());
                if let Some(sink) = self.sink {
                    sink.mark_behind(output_error.clone());
                }
                None
            }
        };
        if terminal {
            self.active = None;
        }
        Ok(ContinueAdaptedPage {
            core: NativePublicationPage::new(source_identity, core),
            output,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn adapt_output(
        &mut self,
        session: &ContinueSessionIdentity,
        source_identity: NativeSourceIdentity,
        observed_revision: String,
        expected_frontier: NativeSafeFrontier,
        next_safe_frontier: NativeSafeFrontier,
        terminal: bool,
        transient: Option<ContinueTransientOutputPayload>,
    ) -> Result<
        (Option<NativeProReplayPage>, Option<ProOutputSinkError>),
        ContinueNativePageAdapterError,
    > {
        let Some(sink) = self.sink else {
            return Ok((None, None));
        };
        let transient = transient.unwrap_or(ContinueTransientOutputPayload {
            observations: Vec::new(),
            failure: None,
        });
        if let Some(failure) = transient.failure {
            let error = ProOutputSinkError::new(
                "continue_output_page_bound",
                format!(
                    "history item {} retained {} outputs/{} bytes: {}",
                    failure.history_ordinal,
                    failure.observed_outputs,
                    failure.observed_bytes,
                    failure.message
                ),
            );
            sink.mark_behind(error.clone());
            return Ok((None, Some(error)));
        }
        let source_key = source_identity.source_identity().to_owned();
        if !self.output_by_source.contains_key(source_key.as_str()) {
            let source = OutputSourceIdentity {
                provider: CaptureProvider::Continue.as_str().to_owned(),
                namespace_id: format!("continue-session:{}", session.0),
                source_id: session.0.clone(),
            };
            let progress = match sink.observe_source(&source) {
                Ok(progress) => progress,
                Err(error) => {
                    sink.mark_behind(error.clone());
                    return Ok((None, Some(error)));
                }
            };
            let state = initial_output_state(
                source,
                progress,
                &observed_revision,
                &expected_frontier,
                sink.materializer_revision(),
            )?;
            self.output_by_source
                .insert(source_key.clone().into_boxed_str(), state);
        }
        let state = self
            .output_by_source
            .get_mut(source_key.as_str())
            .ok_or(ContinueNativePageAdapterError::InvalidOutputFrontier)?;
        if state.observed_revision != observed_revision
            && state.disposition == ProOutputSourceDisposition::AppendOrResume
        {
            state.source_epoch = state
                .source_epoch
                .checked_add(1)
                .ok_or(ContinueNativePageAdapterError::OutputEpochExhausted)?;
            state.expected_source_epoch = state.source_epoch.checked_sub(1);
            state.disposition = ProOutputSourceDisposition::Rewrite;
            state.observed_revision = observed_revision.clone();
            state.resume_through = None;
        }
        if let Some(resume_through) = state.resume_through.as_ref() {
            let next = decode_output_frontier(&next_safe_frontier)?;
            if next.next_page_ordinal < resume_through.next_page_ordinal {
                if terminal {
                    return Err(ContinueNativePageAdapterError::InvalidOutputFrontier);
                }
                return Ok((None, None));
            }
            if &next != resume_through {
                return Err(ContinueNativePageAdapterError::InvalidOutputFrontier);
            }
            state.resume_through = None;
            state.expected_sink_frontier = Some(next_safe_frontier);
            state.disposition = ProOutputSourceDisposition::AppendOrResume;
            return Ok((None, None));
        }
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision,
            parser_revision: CONTINUE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_sink_frontier.clone(),
            observations: transient.observations,
        };
        let accounting = NativePageAccounting {
            logical_units: output.observations.len(),
            conservative_serialized_bytes: estimated_output_page_bytes(
                &source_identity,
                &expected_frontier,
                &next_safe_frontier,
                &output,
            ),
        };
        let replay = match NativeProReplayPage::new_with_source_identity(
            source_identity,
            expected_frontier,
            next_safe_frontier.clone(),
            terminal,
            accounting,
            output,
        ) {
            Ok(replay) => replay,
            Err(error) => {
                let error =
                    ProOutputSinkError::new("continue_output_page_invalid", error.to_string());
                sink.mark_behind(error.clone());
                return Ok((None, Some(error)));
            }
        };
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_sink_frontier = Some(next_safe_frontier);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
        Ok((Some(replay), None))
    }

    fn ensure_source(
        &mut self,
        page: &ContinuePreparedPage,
    ) -> Result<(), ContinueNativePageAdapterError> {
        match (&self.active, page.source.as_deref()) {
            (None, Some(source)) if page.page_ordinal == 0 => {
                self.active = Some(active_source(source));
                Ok(())
            }
            (None, _) => Err(ContinueNativePageAdapterError::MissingSourceAuthority),
            (Some(_), Some(_)) => Err(ContinueNativePageAdapterError::FrontierMismatch),
            (Some(_), None) => Ok(()),
        }
    }
}

fn estimated_output_page_bytes(
    source_identity: &NativeSourceIdentity,
    expected_frontier: &NativeSafeFrontier,
    next_safe_frontier: &NativeSafeFrontier,
    output: &NativeProOutputPage,
) -> usize {
    const FIXED_REPLAY_ENVELOPE_BYTES: usize = 1024;
    let mut bytes = FIXED_REPLAY_ENVELOPE_BYTES
        .saturating_add(source_identity.provider().len())
        .saturating_add(source_identity.source_identity().len())
        .saturating_add(expected_frontier.bytes.len())
        .saturating_add(next_safe_frontier.bytes.len())
        .saturating_add(output.source.provider.len())
        .saturating_add(output.source.namespace_id.len())
        .saturating_add(output.source.source_id.len())
        .saturating_add(output.observed_revision.len())
        .saturating_add(output.parser_revision.len())
        .saturating_add(output.materializer_revision.len())
        .saturating_add(
            output
                .expected_prior_frontier
                .as_ref()
                .map_or(0, |frontier| frontier.bytes.len()),
        );
    for observation in &output.observations {
        bytes = bytes.saturating_add(estimated_output_bytes(observation));
    }
    bytes
}

fn initial_output_state(
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    observed_revision: &str,
    expected_frontier: &NativeSafeFrontier,
    materializer_revision: &str,
) -> Result<OutputState, ContinueNativePageAdapterError> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
            observed_revision: observed_revision.to_owned(),
            resume_through: None,
        });
    };
    let prior = progress
        .cursor
        .as_ref()
        .map(output_safe_frontier)
        .transpose()?;
    let prior_frontier = prior.as_ref().map(decode_output_frontier).transpose()?;
    let revisions_match = progress.parser_revision == CONTINUE_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
        && progress.observed_revision == observed_revision;
    let can_resume_ahead = revisions_match
        && prior_frontier.as_ref().is_some_and(|prior| {
            decode_output_frontier(expected_frontier)
                .is_ok_and(|expected| prior.next_page_ordinal >= expected.next_page_ordinal)
        });
    let rewrite = !revisions_match
        || progress.materializer_revision != materializer_revision
        || (!can_resume_ahead
            && prior
                .as_ref()
                .is_some_and(|prior| prior != expected_frontier));
    Ok(OutputState {
        source,
        source_epoch: if rewrite {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(ContinueNativePageAdapterError::OutputEpochExhausted)?
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
        resume_through: (!rewrite && can_resume_ahead)
            .then_some(prior_frontier)
            .flatten(),
    })
}

fn output_safe_frontier(
    cursor: &OutputNativeCursor,
) -> Result<NativeSafeFrontier, ContinueNativePageAdapterError> {
    if cursor.version != CONTINUE_NATIVE_FRONTIER_VERSION {
        return Err(ContinueNativePageAdapterError::InvalidOutputFrontier);
    }
    NativeSafeFrontier::new(cursor.version, cursor.payload.clone())
        .map_err(ContinueNativePageAdapterError::from)
}

fn decode_output_frontier(
    frontier: &NativeSafeFrontier,
) -> Result<ContinuePageFrontier, ContinueNativePageAdapterError> {
    if frontier.version != CONTINUE_NATIVE_FRONTIER_VERSION {
        return Err(ContinueNativePageAdapterError::InvalidOutputFrontier);
    }
    serde_json::from_slice(&frontier.bytes)
        .map_err(|_| ContinueNativePageAdapterError::InvalidOutputFrontier)
}

fn active_source(source: &ContinuePreparedSource) -> ActiveSource {
    let session = source.session.identity.clone();
    ActiveSource {
        source_identity: NativeSourceIdentity::new(
            CaptureProvider::Continue.as_str(),
            format!("continue-session:{}", session.0),
        ),
        source_revision: format!(
            "{};index={}",
            source.observation.session_revision(),
            source.index_dependency.dependency_revision()
        ),
        session,
        frontier: ContinuePageFrontier::initial(),
    }
}

fn next_history_ordinal(
    page: &ContinuePreparedPage,
    expected: &ContinuePageFrontier,
) -> Result<u64, ContinueNativePageAdapterError> {
    let mut next = expected.next_history_ordinal;
    for event in &page.events {
        if event.identity.history_ordinal < next {
            return Err(ContinueNativePageAdapterError::FrontierMismatch);
        }
        next = event
            .identity
            .history_ordinal
            .checked_add(1)
            .ok_or(ContinueNativePageAdapterError::FrontierMismatch)?;
    }
    if !page.terminal {
        return Ok(next);
    }
    let authority = page
        .authority
        .as_ref()
        .ok_or(ContinueNativePageAdapterError::InvalidTerminalAuthority)?;
    let observed = authority
        .observed_history_items
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(ContinueNativePageAdapterError::InvalidTerminalAuthority)?;
    if observed < next {
        return Err(ContinueNativePageAdapterError::InvalidTerminalAuthority);
    }
    Ok(observed)
}

fn advance_prefix(expected: &ContinuePageFrontier, page: &ContinuePreparedPage) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CONTINUE_FRONTIER_HASH_DOMAIN);
    hasher.update(expected.prefix_semantic_sha256);
    if let Some(source) = page.source.as_deref() {
        hash_field(&mut hasher, source.session.metadata_hash.as_bytes());
    }
    for event in &page.events {
        hasher.update(event.identity.history_ordinal.to_le_bytes());
        hash_field(&mut hasher, event.content_hash.as_bytes());
    }
    if let Some(authority) = page.authority.as_ref() {
        hash_authority(&mut hasher, authority);
    }
    hasher.finalize().into()
}

fn hash_authority(hasher: &mut Sha256, authority: &ContinueGenerationAuthority) {
    hasher.update([authority.completeness as u8]);
    hasher.update(
        authority
            .observed_history_items
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(
        u64::try_from(authority.retained_events)
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(
        u64::try_from(authority.rejected_items)
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn safe_frontier(
    frontier: &ContinuePageFrontier,
) -> Result<NativeSafeFrontier, ContinueNativePageAdapterError> {
    let encoded = serde_json::to_vec(frontier)
        .map_err(|_| ContinueNativePageAdapterError::FrontierEncoding)?;
    NativeSafeFrontier::new(CONTINUE_NATIVE_FRONTIER_VERSION, encoded)
        .map_err(ContinueNativePageAdapterError::from)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContinueNativeStoreCursor {
    pub(crate) version: u32,
    pub(crate) source_identity: String,
    pub(crate) source_revision: String,
    pub(crate) frontier: ContinuePageFrontier,
    pub(crate) terminal: bool,
    pub(crate) generation: u64,
    pub(crate) rejected_records: u64,
}

impl ContinueNativeStoreCursor {
    pub(crate) const VERSION: u32 = 1;

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
    fn store_cursor_round_trips_without_whole_json_cursor_types() {
        let cursor = ContinueNativeStoreCursor {
            version: ContinueNativeStoreCursor::VERSION,
            source_identity: "continue-session:session".to_owned(),
            source_revision: "revision".to_owned(),
            frontier: ContinuePageFrontier {
                version: 1,
                next_page_ordinal: 2,
                next_history_ordinal: 17,
                prefix_semantic_sha256: [7; 32],
            },
            terminal: true,
            generation: 4,
            rejected_records: 3,
        };
        let encoded = cursor.encode().expect("encode");
        assert_eq!(
            ContinueNativeStoreCursor::decode(&encoded).expect("decode"),
            cursor
        );
    }
}

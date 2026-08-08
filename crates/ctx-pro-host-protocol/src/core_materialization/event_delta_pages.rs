use serde::{Deserialize, Serialize};

use super::{
    core_source_delta_exact_eq, invalid_contract, validate_encoded_bound, validate_sha256,
    CoreEventDelta, CoreEventDeltaPage, CoreEventDeltaPageApplied, ErrorClass, ProtocolError,
    SourceKey, MAX_CORE_CONTROL_WIRE_BYTES,
};

pub const MAX_CORE_EVENT_DELTA_PAGES: usize = 16;
pub const MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES: usize = 68 * 1024 * 1024;
/// Maximum helper-side prepared output retained while atomically applying one
/// request. Prepared output is not a wire DTO, so implementations account it
/// against this public contract before committing or acknowledging the pages.
pub const MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyCoreEventDeltaPagesRequest {
    pub pages: Vec<CoreEventDeltaPage>,
}

impl ApplyCoreEventDeltaPagesRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.validate_and_encoded_len().map(|_| ())
    }

    /// Validates an ordered envelope of prepared pages using the complete
    /// public page contract, including each page's compact JSON wire bound.
    pub fn validate_prepared_envelope<'a>(
        pages: impl ExactSizeIterator<Item = &'a CoreEventDeltaPage>,
    ) -> Result<(), ProtocolError> {
        let pages = pages.collect::<Vec<_>>();
        validate_event_delta_page_envelope(pages.iter().copied()).map(|_| ())
    }

    /// Captures only the fields needed to validate the ordered batch
    /// acknowledgement after the complete request moves into transport.
    pub fn acknowledgement_identity(
        &self,
    ) -> Result<CoreEventDeltaPagesAcknowledgementIdentity, ProtocolError> {
        self.validate()?;
        let first = &self.pages[0];
        let pages = self
            .pages
            .iter()
            .map(CoreEventDeltaPageAcknowledgementIdentity::from_page)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CoreEventDeltaPagesAcknowledgementIdentity {
            materialization_id: first.materialization_id.clone(),
            core_generation_id: first.core_generation_id.clone(),
            pages,
        })
    }

    /// Captures acknowledgement state for pages already prepared from a
    /// validated, generation-pinned Core page and measured by their exact
    /// compact request encoding.
    ///
    /// The helper still runs [`Self::validate`] after decoding the wire request;
    /// this host-side seam avoids re-traversing complete records solely to keep
    /// acknowledgement metadata after transport.
    pub fn acknowledgement_identity_for_prepared_request(
        &self,
        encoded_request_bytes: usize,
    ) -> Result<CoreEventDeltaPagesAcknowledgementIdentity, ProtocolError> {
        let validated_encoded_request_bytes = self.validate_and_encoded_len()?;
        if encoded_request_bytes != validated_encoded_request_bytes {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "prepared Core event delta page batch length does not match canonical JSON",
            ));
        }
        if encoded_request_bytes > MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core event delta page batch exceeds its aggregate wire bound",
            ));
        }
        let first = &self.pages[0];
        let pages = self
            .pages
            .iter()
            .map(CoreEventDeltaPageAcknowledgementIdentity::from_page)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CoreEventDeltaPagesAcknowledgementIdentity {
            materialization_id: first.materialization_id.clone(),
            core_generation_id: first.core_generation_id.clone(),
            pages,
        })
    }

    fn validate_and_encoded_len(&self) -> Result<usize, ProtocolError> {
        validate_event_delta_page_envelope(self.pages.iter())
    }
}

fn validate_event_delta_page_envelope<'a>(
    mut pages: impl ExactSizeIterator<Item = &'a CoreEventDeltaPage>,
) -> Result<usize, ProtocolError> {
    if pages.len() == 0 || pages.len() > MAX_CORE_EVENT_DELTA_PAGES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "Core event delta page batch has an invalid page count",
        ));
    }

    let first = pages
        .next()
        .expect("non-empty Core event envelope was checked above");
    let mut encoded_page_bytes = Vec::with_capacity(pages.len() + 1);
    encoded_page_bytes.push(first.validate_and_encoded_len()?);
    let mut prior_page_index = first.page_index;
    let mut prior_event_id = first.deltas.last().map(|delta| delta.event_id().digest());
    let mut prior_terminal = first.terminal;
    let mut prior_source = first.reconciliation.delta.source().identity().digest();
    let mut prior_materialize_index = first.reconciliation.materialize_index;
    let mut prior_reconciliation = &first.reconciliation.delta;
    let mut seen_sources = vec![prior_source];
    for page in pages {
        encoded_page_bytes.push(page.validate_and_encoded_len()?);
        if page.materialization_id != first.materialization_id
            || page.core_generation_id != first.core_generation_id
        {
            return Err(batch_sequence_error());
        }
        let source = page.reconciliation.delta.source().identity().digest();
        let same_source = prior_source == source;
        if same_source {
            if prior_terminal
                || prior_page_index.checked_add(1) != Some(page.page_index)
                || prior_materialize_index != page.reconciliation.materialize_index
                || !core_source_delta_exact_eq(&page.reconciliation.delta, prior_reconciliation)
            {
                return Err(batch_sequence_error());
            }
        } else {
            if seen_sources.contains(&source)
                || !prior_terminal
                || page.page_index != 0
                || prior_materialize_index >= page.reconciliation.materialize_index
            {
                return Err(batch_sequence_error());
            }
            seen_sources.push(source);
            prior_event_id = None;
        }
        if let Some(first_delta) = page.deltas.first() {
            let current = first_delta.event_id().digest();
            if prior_event_id.is_some_and(|prior| prior >= current) {
                return Err(batch_sequence_error());
            }
        }
        if let Some(last_delta) = page.deltas.last() {
            prior_event_id = Some(last_delta.event_id().digest());
        }
        prior_page_index = page.page_index;
        prior_terminal = page.terminal;
        prior_source = source;
        prior_materialize_index = page.reconciliation.materialize_index;
        prior_reconciliation = &page.reconciliation.delta;
    }

    let encoded_request_bytes =
        checked_core_event_delta_pages_request_len(encoded_page_bytes.into_iter())?;
    if encoded_request_bytes > MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "Core event delta page batch exceeds its aggregate wire bound",
        ));
    }
    Ok(encoded_request_bytes)
}

pub(super) fn checked_core_event_delta_pages_request_len(
    encoded_page_bytes: impl Iterator<Item = usize>,
) -> Result<usize, ProtocolError> {
    encoded_page_bytes.enumerate().try_fold(
        b"{\"pages\":[]}".len(),
        |total, (index, page_bytes)| {
            total
                .checked_add(usize::from(index != 0))
                .and_then(|total| total.checked_add(page_bytes))
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "Core event delta page batch length overflowed",
                    )
                })
        },
    )
}

fn batch_sequence_error() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::Sequence,
        "Core event delta page envelope must contain materialize-index-ordered source-pinned contiguous sub-batches",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreEventDeltaPagesAcknowledgementIdentity {
    materialization_id: String,
    core_generation_id: String,
    pages: Vec<CoreEventDeltaPageAcknowledgementIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CoreEventDeltaPageAcknowledgementIdentity {
    source: SourceKey,
    page_index: u32,
    additions: u32,
    replacements: u32,
    tombstones: u32,
    terminal: bool,
}

impl CoreEventDeltaPageAcknowledgementIdentity {
    fn from_page(page: &CoreEventDeltaPage) -> Result<Self, ProtocolError> {
        let count = |matches: fn(&CoreEventDelta) -> bool| {
            u32::try_from(page.deltas.iter().filter(|delta| matches(delta)).count()).map_err(|_| {
                ProtocolError::new(
                    ErrorClass::Bounds,
                    "Core event delta acknowledgement count overflowed",
                )
            })
        };
        Ok(Self {
            source: page.reconciliation.delta.source().clone(),
            page_index: page.page_index,
            additions: count(|delta| matches!(delta, CoreEventDelta::Added(_)))?,
            replacements: count(|delta| matches!(delta, CoreEventDelta::Replaced(_)))?,
            tombstones: count(|delta| matches!(delta, CoreEventDelta::Tombstoned(_)))?,
            terminal: page.terminal,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreEventDeltaPagesApplied {
    pub pages: Vec<CoreEventDeltaPageApplied>,
}

impl CoreEventDeltaPagesApplied {
    pub fn validate_for(
        &self,
        request: &ApplyCoreEventDeltaPagesRequest,
    ) -> Result<(), ProtocolError> {
        self.validate_for_identity(&request.acknowledgement_identity()?)
    }

    pub fn validate_for_identity(
        &self,
        identity: &CoreEventDeltaPagesAcknowledgementIdentity,
    ) -> Result<(), ProtocolError> {
        if self.pages.len() != identity.pages.len() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core event delta page batch acknowledgement changed its page count",
            ));
        }
        for (applied, expected) in self.pages.iter().zip(&identity.pages) {
            validate_sha256(&applied.materialization_id, "Core materialization ID")?;
            validate_sha256(&applied.core_generation_id, "Core generation ID")?;
            applied.source.validate_contract().map_err(|error| {
                invalid_contract("Core event delta acknowledgement source", error)
            })?;
            if applied.materialization_id != identity.materialization_id
                || applied.core_generation_id != identity.core_generation_id
                || !applied.source.exact_descriptor_eq(&expected.source)
                || applied.page_index != expected.page_index
                || applied.additions != expected.additions
                || applied.replacements != expected.replacements
                || applied.tombstones != expected.tombstones
                || applied.terminal != expected.terminal
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core event delta page batch acknowledgement does not match its ordered request CAS",
                ));
            }
        }
        validate_encoded_bound(
            self,
            MAX_CORE_CONTROL_WIRE_BYTES,
            "Core event delta page batch acknowledgement exceeds its wire bound",
        )
    }
}

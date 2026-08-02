use super::validation::{core_record_content_bytes, encoded_len};
use super::{
    validate_core_event_delta_page_header, CoreEventDelta, CoreEventDeltaPage, CoreSourceDelta,
    CoreSourceReconciliation, MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_ITEMS, MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES,
};
use crate::{ErrorClass, ProtocolError};

/// Incrementally builds exact, bounded Core event delta pages.
///
/// Each incoming delta is contract-validated, content-charged, and JSON-sized
/// once. Completed pages still require [`CoreEventDeltaPage::validate`] as the
/// authoritative final check immediately before transport.
#[derive(Debug)]
pub struct CoreEventDeltaPageBuilder {
    materialization_id: String,
    core_generation_id: String,
    reconciliation: CoreSourceReconciliation,
    page_index: u32,
    deltas: Vec<CoreEventDelta>,
    content_bytes: usize,
    encoded_delta_items_bytes: usize,
    empty_nonterminal_wire_bytes: usize,
    maximum_content_bytes: usize,
    maximum_wire_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
struct DeltaCharge {
    content_bytes: usize,
    encoded_bytes: usize,
}

impl CoreEventDeltaPageBuilder {
    pub fn new(
        materialization_id: impl Into<String>,
        core_generation_id: impl Into<String>,
        reconciliation: CoreSourceReconciliation,
        first_page_index: u32,
    ) -> Result<Self, ProtocolError> {
        Self::with_limits(
            materialization_id.into(),
            core_generation_id.into(),
            reconciliation,
            first_page_index,
            MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
            MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES,
        )
    }

    #[cfg(test)]
    pub(super) fn with_test_limits(
        materialization_id: impl Into<String>,
        core_generation_id: impl Into<String>,
        reconciliation: CoreSourceReconciliation,
        first_page_index: u32,
        maximum_content_bytes: usize,
        maximum_wire_bytes: usize,
    ) -> Result<Self, ProtocolError> {
        Self::with_limits(
            materialization_id.into(),
            core_generation_id.into(),
            reconciliation,
            first_page_index,
            maximum_content_bytes,
            maximum_wire_bytes,
        )
    }

    fn with_limits(
        materialization_id: String,
        core_generation_id: String,
        reconciliation: CoreSourceReconciliation,
        first_page_index: u32,
        maximum_content_bytes: usize,
        maximum_wire_bytes: usize,
    ) -> Result<Self, ProtocolError> {
        let mut builder = Self {
            materialization_id,
            core_generation_id,
            reconciliation,
            page_index: first_page_index,
            deltas: Vec::with_capacity(MAX_CORE_EVENT_DELTA_PAGE_ITEMS),
            content_bytes: 0,
            encoded_delta_items_bytes: 0,
            empty_nonterminal_wire_bytes: 0,
            maximum_content_bytes,
            maximum_wire_bytes,
        };
        validate_core_event_delta_page_header(
            &builder.materialization_id,
            &builder.core_generation_id,
            &builder.reconciliation,
        )?;
        builder.empty_nonterminal_wire_bytes = builder.empty_wire_bytes(false)?;
        Ok(builder)
    }

    /// Adds one delta and returns a completed nonterminal page when this delta
    /// starts the next exact page.
    pub fn push(
        &mut self,
        delta: CoreEventDelta,
    ) -> Result<Option<CoreEventDeltaPage>, ProtocolError> {
        let charge = self.charge(&delta)?;

        if self.deltas.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
            let completed = self.advance_page()?;
            self.push_singleton(delta, charge)?;
            return Ok(Some(completed));
        }

        if let Some(prior) = self.deltas.last() {
            if prior.event_id().digest() >= delta.event_id().digest() {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "Core event deltas must be strictly ordered by event identity",
                ));
            }
        }

        if self.fits(charge) {
            self.push_charged(delta, charge);
            return Ok(None);
        }
        if self.deltas.is_empty() {
            return Err(oversized_singleton());
        }

        let completed = self.advance_page()?;
        self.push_singleton(delta, charge)?;
        Ok(Some(completed))
    }

    /// Returns the final terminal page. Empty terminal pages are retained for
    /// source reconciliations that contain no event mutations.
    pub fn finish(self) -> CoreEventDeltaPage {
        self.into_page(true)
    }

    fn charge(&self, delta: &CoreEventDelta) -> Result<DeltaCharge, ProtocolError> {
        let source = self.reconciliation.delta.source();
        delta.validate_for_source(source)?;
        if matches!(&self.reconciliation.delta, CoreSourceDelta::Removed(_))
            && !matches!(delta, CoreEventDelta::Tombstoned(_))
        {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "removed Core sources accept only event tombstones",
            ));
        }
        let content_bytes = delta
            .record()
            .map(core_record_content_bytes)
            .transpose()?
            .unwrap_or(0);
        let encoded_bytes = encoded_len(delta)?;
        Ok(DeltaCharge {
            content_bytes,
            encoded_bytes,
        })
    }

    fn fits(&self, charge: DeltaCharge) -> bool {
        let separator_bytes = usize::from(!self.deltas.is_empty());
        self.content_bytes
            .checked_add(charge.content_bytes)
            .is_some_and(|bytes| bytes <= self.maximum_content_bytes)
            && self
                .empty_nonterminal_wire_bytes
                .checked_add(self.encoded_delta_items_bytes)
                .and_then(|bytes| bytes.checked_add(separator_bytes))
                .and_then(|bytes| bytes.checked_add(charge.encoded_bytes))
                .is_some_and(|bytes| bytes <= self.maximum_wire_bytes)
    }

    fn push_singleton(
        &mut self,
        delta: CoreEventDelta,
        charge: DeltaCharge,
    ) -> Result<(), ProtocolError> {
        if !self.fits(charge) {
            return Err(oversized_singleton());
        }
        self.push_charged(delta, charge);
        Ok(())
    }

    fn push_charged(&mut self, delta: CoreEventDelta, charge: DeltaCharge) {
        if !self.deltas.is_empty() {
            self.encoded_delta_items_bytes += 1;
        }
        self.content_bytes += charge.content_bytes;
        self.encoded_delta_items_bytes += charge.encoded_bytes;
        self.deltas.push(delta);
    }

    fn advance_page(&mut self) -> Result<CoreEventDeltaPage, ProtocolError> {
        let next_page_index = self.page_index.checked_add(1).ok_or_else(|| {
            ProtocolError::new(ErrorClass::Bounds, "Core event delta page index overflowed")
        })?;
        let completed = CoreEventDeltaPage {
            materialization_id: self.materialization_id.clone(),
            core_generation_id: self.core_generation_id.clone(),
            reconciliation: self.reconciliation.clone(),
            page_index: self.page_index,
            terminal: false,
            deltas: std::mem::take(&mut self.deltas),
        };
        self.page_index = next_page_index;
        self.content_bytes = 0;
        self.encoded_delta_items_bytes = 0;
        self.empty_nonterminal_wire_bytes = self.empty_wire_bytes(false)?;
        Ok(completed)
    }

    fn empty_wire_bytes(&self, terminal: bool) -> Result<usize, ProtocolError> {
        encoded_len(&CoreEventDeltaPage {
            materialization_id: self.materialization_id.clone(),
            core_generation_id: self.core_generation_id.clone(),
            reconciliation: self.reconciliation.clone(),
            page_index: self.page_index,
            terminal,
            deltas: Vec::new(),
        })
    }

    fn into_page(self, terminal: bool) -> CoreEventDeltaPage {
        CoreEventDeltaPage {
            materialization_id: self.materialization_id,
            core_generation_id: self.core_generation_id,
            reconciliation: self.reconciliation,
            page_index: self.page_index,
            terminal,
            deltas: self.deltas,
        }
    }
}

fn oversized_singleton() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::Bounds,
        "one Core event delta exceeds its page bound",
    )
}

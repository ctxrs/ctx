//! Authenticated runtime-only source reconciliation selection.

use std::io::Write;

use sha2::{Digest as _, Sha256};

use crate::graph::segment_state::SegmentCandidateControl;
use crate::protocol::{CoreSourceDelta, CoreSourceReconciliation, MAX_CORE_SOURCE_STATES};

use super::SegmentMaterializerError;
use super::model::CandidateState;

pub(super) const RECONCILIATION_CURSOR_ENTRY_BYTES: usize = 64;
pub(super) const MAX_RECONCILIATION_CURSOR_ENTRIES: usize = 2 * MAX_CORE_SOURCE_STATES;
pub(super) const MAX_RECONCILIATION_CURSOR_BYTES: usize =
    MAX_RECONCILIATION_CURSOR_ENTRIES * RECONCILIATION_CURSOR_ENTRY_BYTES;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReconciliationEntry {
    source_identity_sha256: [u8; 32],
    delta_sha256: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<ReconciliationEntry>() == 64);

#[derive(Clone, Debug, PartialEq, Eq)]
struct CursorOwner {
    materialization_id: String,
    core_generation_id: String,
    graph_generation: u64,
    materializer_revision: String,
}

#[derive(Debug)]
pub(super) struct PreparedCursorAppend {
    expected_entry_count: usize,
    entries: Vec<ReconciliationEntry>,
}

#[derive(Debug)]
pub(super) struct ReconciliationCursor {
    owner: CursorOwner,
    entries: Vec<ReconciliationEntry>,
    entry_limit: usize,
}

impl ReconciliationCursor {
    pub(super) fn empty_for(
        state: &CandidateState,
        active_source_count: usize,
    ) -> Result<Self, SegmentMaterializerError> {
        let candidate = &state.control;
        let owner = CursorOwner::from_candidate(candidate);
        let entry_limit = entry_limit(candidate, active_source_count)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(entry_limit)
            .map_err(|_| SegmentMaterializerError::Bounds)?;
        Ok(Self {
            owner,
            entries,
            entry_limit,
        })
    }

    pub(super) fn owner_matches(&self, state: &CandidateState, active_source_count: usize) -> bool {
        let candidate = &state.control;
        Some(CursorOwner::from_candidate(candidate))
            .zip(entry_limit(candidate, active_source_count).ok())
            .is_some_and(|(owner, limit)| self.owner == owner && self.entry_limit == limit)
    }

    pub(super) fn prepare_append(
        &self,
        reconciliations: &[CoreSourceReconciliation],
    ) -> Result<PreparedCursorAppend, SegmentMaterializerError> {
        let next_len = self
            .entries
            .len()
            .checked_add(reconciliations.len())
            .ok_or(SegmentMaterializerError::Bounds)?;
        if next_len > self.entry_limit || next_len > MAX_RECONCILIATION_CURSOR_ENTRIES {
            return Err(SegmentMaterializerError::Bounds);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(reconciliations.len())
            .map_err(|_| SegmentMaterializerError::Bounds)?;
        for (ordinal, reconciliation) in reconciliations.iter().enumerate() {
            let expected_index = self
                .entries
                .len()
                .checked_add(ordinal)
                .and_then(|index| u32::try_from(index).ok())
                .ok_or(SegmentMaterializerError::Bounds)?;
            if reconciliation.materialize_index != expected_index {
                return Err(SegmentMaterializerError::Corrupt(
                    "source reconciliation cursor indices are not contiguous",
                ));
            }
            let entry = reconciliation_entry(&reconciliation.delta)?;
            entries.push(entry);
        }
        Ok(PreparedCursorAppend {
            expected_entry_count: self.entries.len(),
            entries,
        })
    }

    pub(super) fn commit_append(
        &mut self,
        prepared: PreparedCursorAppend,
    ) -> Result<(), SegmentMaterializerError> {
        if self.entries.len() != prepared.expected_entry_count
            || self.entries.len().saturating_add(prepared.entries.len()) > self.entry_limit
            || self.entries.capacity() < self.entries.len().saturating_add(prepared.entries.len())
        {
            return Err(SegmentMaterializerError::Corrupt(
                "source reconciliation cursor reservation changed",
            ));
        }
        self.entries.extend(prepared.entries);
        Ok(())
    }

    pub(super) fn require_current(
        &self,
        materialize_index: u32,
        reconciliation: &CoreSourceReconciliation,
    ) -> Result<(), SegmentMaterializerError> {
        let index =
            usize::try_from(materialize_index).map_err(|_| SegmentMaterializerError::Bounds)?;
        let expected = self
            .entries
            .get(index)
            .ok_or(SegmentMaterializerError::Corrupt(
                "current source reconciliation is absent from the runtime cursor",
            ))?;
        let requested = reconciliation_entry(&reconciliation.delta)?;
        if *expected != requested {
            return Err(SegmentMaterializerError::Conflict);
        }
        Ok(())
    }

    pub(super) fn require_committed(
        &self,
        reconciliation: &CoreSourceReconciliation,
    ) -> Result<(), SegmentMaterializerError> {
        let requested = reconciliation_entry(&reconciliation.delta)?;
        let index = usize::try_from(reconciliation.materialize_index)
            .map_err(|_| SegmentMaterializerError::Bounds)?;
        if self.entries.get(index) != Some(&requested) {
            return Err(SegmentMaterializerError::Conflict);
        }
        Ok(())
    }

    pub(super) const fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn reserved_bytes(&self) -> Result<usize, SegmentMaterializerError> {
        cursor_payload_bytes(self.entry_limit)
    }
}

impl CursorOwner {
    fn from_candidate(candidate: &crate::graph::segment_state::SegmentCandidateControl) -> Self {
        Self {
            materialization_id: candidate.materialization_id.clone(),
            core_generation_id: candidate.head.core_generation_id.clone(),
            graph_generation: candidate.graph_generation,
            materializer_revision: candidate.materializer_revision.clone(),
        }
    }
}

fn entry_limit(
    candidate: &SegmentCandidateControl,
    active_source_count: usize,
) -> Result<usize, SegmentMaterializerError> {
    let candidate_source_count = usize::try_from(candidate.head.source_count)
        .map_err(|_| SegmentMaterializerError::Bounds)?;
    bounded_entry_limit(
        candidate_source_count,
        active_source_count,
        candidate.changed_sources,
        candidate.removed_sources,
    )
}

fn bounded_entry_limit(
    candidate_source_count: usize,
    active_source_count: usize,
    changed_sources: u32,
    removed_sources: u32,
) -> Result<usize, SegmentMaterializerError> {
    if candidate_source_count > MAX_CORE_SOURCE_STATES
        || active_source_count > MAX_CORE_SOURCE_STATES
        || usize::try_from(changed_sources)
            .ok()
            .is_none_or(|changed| changed > candidate_source_count)
        || usize::try_from(removed_sources)
            .ok()
            .is_none_or(|removed| removed > active_source_count)
    {
        return Err(SegmentMaterializerError::Bounds);
    }
    let limit = candidate_source_count
        .checked_add(active_source_count)
        .ok_or(SegmentMaterializerError::Bounds)?;
    if limit > MAX_RECONCILIATION_CURSOR_ENTRIES {
        return Err(SegmentMaterializerError::Bounds);
    }
    let _ = cursor_payload_bytes(limit)?;
    Ok(limit)
}

fn cursor_payload_bytes(entry_count: usize) -> Result<usize, SegmentMaterializerError> {
    let bytes = entry_count
        .checked_mul(RECONCILIATION_CURSOR_ENTRY_BYTES)
        .ok_or(SegmentMaterializerError::Bounds)?;
    if bytes > MAX_RECONCILIATION_CURSOR_BYTES {
        return Err(SegmentMaterializerError::Bounds);
    }
    Ok(bytes)
}

fn reconciliation_entry(
    delta: &CoreSourceDelta,
) -> Result<ReconciliationEntry, SegmentMaterializerError> {
    let mut digest = Sha256::new();
    serde_json::to_writer(DigestWriter(&mut digest), delta)
        .map_err(|_| SegmentMaterializerError::Encoding)?;
    Ok(ReconciliationEntry {
        source_identity_sha256: delta.source().identity().digest(),
        delta_sha256: digest.finalize().into(),
    })
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "reconciliation_cursor/tests.rs"]
mod tests;

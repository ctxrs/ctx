use std::io;

use super::*;

pub(super) struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
    exceeded: bool,
}

impl BoundedWriter {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            exceeded: false,
        }
    }
}

impl io::Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(length) = self.bytes.len().checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, "state bound"));
        };
        if length > self.maximum {
            self.exceeded = true;
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, "state bound"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SegmentCompletedControl {
    pub fn validate(&self) -> Result<(), CoreStoreError> {
        validate_materializer_revision(&self.materializer_revision)?;
        validate_contract_text(&self.schema_contract)?;
        validate_contract_text(&self.semantics_contract)?;
        validate_contract_text(&self.evidence_contract)?;
        validate_contract_text(&self.core_record_contract)?;
        validate_event_count_bound(self.event_count)?;
        validate_coverage_bound(&self.coverage)?;
        validate_lower_sha256(&self.publication_semantics_sha256)?;
        match (&self.receipt, &self.head) {
            (None, None) => {
                if self.graph_generation != 0
                    || self.event_count != 0
                    || self.materialization_id.is_some()
                    || self.expected_prior_receipt.is_some()
                    || self.finish_request_sha256.is_some()
                    || self.coverage != SegmentCoreCoverage::default()
                {
                    return Err(CoreStoreError::Backend);
                }
            }
            (Some(receipt), Some(head)) => {
                receipt
                    .validate_for_head(head)
                    .map_err(|_| CoreStoreError::Backend)?;
                if self.graph_generation == 0
                    || self.event_count != receipt.event_count
                    || self
                        .materialization_id
                        .as_deref()
                        .is_none_or(|value| validate_lower_sha256(value).is_err())
                    || self
                        .finish_request_sha256
                        .as_deref()
                        .is_none_or(|value| validate_lower_sha256(value).is_err())
                {
                    return Err(CoreStoreError::Backend);
                }
                if let Some(prior) = &self.expected_prior_receipt {
                    prior.validate().map_err(|_| CoreStoreError::Backend)?;
                }
            }
            _ => return Err(CoreStoreError::Backend),
        }
        Ok(())
    }
}

impl SegmentCorePageOutput {
    pub(super) fn validate_bounds(&self) -> Result<(), CoreStoreError> {
        if self.mutations.len() > crate::protocol::MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
            return Err(CoreStoreError::Bounds);
        }
        for mutation in &self.mutations {
            validate_publication_mutation_bounds(mutation)?;
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), CoreStoreError> {
        self.validate_bounds()?;
        validate_lower_sha256(&self.materialization_id)?;
        validate_lower_sha256(&self.request_sha256)?;
        if self.graph_generation == 0 {
            return Err(CoreStoreError::Backend);
        }
        for mutation in &self.mutations {
            validate_publication_mutation(mutation)?;
        }
        self.effect
            .validate()
            .map_err(|_| CoreStoreError::Backend)?;
        if self.effect.materialization_id != self.materialization_id {
            return Err(CoreStoreError::Backend);
        }
        Ok(())
    }
}

pub(super) fn bounded_json(
    value: &impl Serialize,
    maximum: usize,
) -> Result<Vec<u8>, CoreStoreError> {
    let mut writer = BoundedWriter::new(maximum);
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.exceeded {
            CoreStoreError::Bounds
        } else {
            CoreStoreError::Backend
        });
    }
    Ok(writer.bytes)
}

fn validate_prepared_unit_bounds(unit: &SegmentPreparedUnit) -> Result<(), CoreStoreError> {
    if unit.stable_entities.len() > MAX_SEGMENT_PREPARED_ENTITIES_PER_EVENT
        || unit.facts.len() > MAX_SEGMENT_PREPARED_FACTS_PER_EVENT
        || unit.facts.iter().any(|fact| {
            fact.evidence.len() > MAX_SEGMENT_FACT_EVIDENCE_ITEMS
                || fact.attributes.len() > MAX_SEGMENT_FACT_ATTRIBUTES
        })
    {
        return Err(CoreStoreError::Bounds);
    }
    bounded_json(unit, MAX_SEGMENT_PREPARED_UNIT_BYTES).map(|_| ())
}

pub(super) fn validate_prepared_unit(
    unit: &SegmentPreparedUnit,
    event_id: StableEntityId,
) -> Result<(), CoreStoreError> {
    validate_prepared_unit_bounds(unit)?;
    validate_identifier(&unit.origin_event_id)?;
    if unit.origin_event_id != event_id.to_string() || !unit.stable_entities.contains(&event_id) {
        return Err(CoreStoreError::Backend);
    }
    for entity in &unit.stable_entities {
        entity
            .validate_contract()
            .map_err(|_| CoreStoreError::Backend)?;
    }
    if !unit.facts.is_empty() && unit.evidence.is_none() {
        return Err(CoreStoreError::Backend);
    }
    if let Some(evidence) = &unit.evidence
        && (!evidence.citation.is_usable() || evidence.citation.event_id != event_id)
    {
        return Err(CoreStoreError::Backend);
    }
    validate_coverage_bound(&unit.coverage)
}

pub(super) fn validate_prepared_event(event: &SegmentPreparedEvent) -> Result<(), CoreStoreError> {
    event
        .event_identity
        .validate_contract()
        .map_err(|_| CoreStoreError::Backend)?;
    if event.event_identity.entity_kind() != StableEntityKind::Event
        || event.owner.event_id != event.event_identity.to_string()
    {
        return Err(CoreStoreError::Backend);
    }
    validate_owner(&event.owner)?;
    validate_lower_sha256(&event.core_record_sha256)?;
    validate_lower_sha256(&event.core_record_leaf_sha256)?;
    validate_prepared_unit(&event.prepared, event.event_identity)
}

fn validate_publication_mutation_bounds(
    mutation: &SegmentPublicationMutation,
) -> Result<(), CoreStoreError> {
    match mutation {
        SegmentPublicationMutation::Added(event) => validate_prepared_unit_bounds(&event.prepared),
        SegmentPublicationMutation::Replaced { replacement, .. } => {
            validate_prepared_unit_bounds(&replacement.prepared)
        }
        SegmentPublicationMutation::Tombstoned(_) => Ok(()),
    }
}

fn validate_publication_mutation(
    mutation: &SegmentPublicationMutation,
) -> Result<(), CoreStoreError> {
    match mutation {
        SegmentPublicationMutation::Added(event) => validate_prepared_event(event),
        SegmentPublicationMutation::Replaced {
            tombstone,
            replacement,
        } => {
            validate_tombstone(tombstone)?;
            validate_prepared_event(replacement)?;
            // The direct session owns replacement admission. Rebuilds may
            // retain identical Core bytes; source rewrites may move a stable
            // event. Each side carries its own sequence and digest.
            if tombstone.owner.source_id != replacement.owner.source_id {
                return Err(CoreStoreError::Backend);
            }
            Ok(())
        }
        SegmentPublicationMutation::Tombstoned(tombstone) => validate_tombstone(tombstone),
    }
}

fn validate_tombstone(tombstone: &SegmentPublicationTombstone) -> Result<(), CoreStoreError> {
    validate_owner(&tombstone.owner)?;
    validate_lower_sha256(&tombstone.prior_core_record_sha256)?;
    validate_lower_sha256(&tombstone.prior_event_state_sha256)
}

fn validate_owner(owner: &SegmentEventOwner) -> Result<(), CoreStoreError> {
    validate_identifier(&owner.source_id)?;
    validate_identifier(&owner.event_id)?;
    validate_identifier(&owner.direct_session_id)?;
    if let Some(root_session_id) = owner.root_session_id.as_deref() {
        validate_identifier(root_session_id)?;
    }
    Ok(())
}

pub(super) fn validate_coverage_bound(
    coverage: &SegmentCoreCoverage,
) -> Result<(), CoreStoreError> {
    let maximum = MAX_SEGMENT_CORE_EVENTS as u64;
    if [
        coverage.repository_candidate_events,
        coverage.logical_binding_events,
        coverage.certified_live_root_access_events,
        coverage.file_evidence_events,
        coverage.exact_commit_evidence_events,
        coverage.exact_pull_request_evidence_events,
    ]
    .into_iter()
    .any(|count| count > maximum)
    {
        return Err(CoreStoreError::Bounds);
    }
    Ok(())
}

pub(super) fn validate_event_count_bound(event_count: u64) -> Result<(), CoreStoreError> {
    if event_count > MAX_SEGMENT_CORE_EVENTS as u64 {
        Err(CoreStoreError::Bounds)
    } else {
        Ok(())
    }
}

pub(super) fn validate_lower_sha256(value: &str) -> Result<(), CoreStoreError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(CoreStoreError::Backend)
    }
}

fn validate_identifier(value: &str) -> Result<(), CoreStoreError> {
    if value.is_empty() || value.len() > 4 * 1024 || value.chars().any(char::is_control) {
        Err(CoreStoreError::Bounds)
    } else {
        Ok(())
    }
}

fn validate_materializer_revision(value: &str) -> Result<(), CoreStoreError> {
    if value.is_empty() || value.len() > crate::protocol::MAX_CORE_MATERIALIZER_REVISION_BYTES {
        Err(CoreStoreError::Bounds)
    } else {
        Ok(())
    }
}

fn validate_contract_text(value: &str) -> Result<(), CoreStoreError> {
    if value.is_empty() || value.len() > 512 {
        Err(CoreStoreError::Backend)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;

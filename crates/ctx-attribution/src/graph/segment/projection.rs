use std::collections::{BTreeMap, BTreeSet};
use std::io;

use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::envelope::{Confidence, Fact, FactState, ResourceRef};
use crate::graph::identity::GraphRecordId;
use crate::graph::is_serving_fact_family;
use crate::ingest::{PreparedCoreProjectionBatch, PreparedCoreUnit};
use crate::protocol::{ResourceKind, StableEntityId};

use super::model::ServingRecordQueryExt as _;
use super::model::{
    AttributeValue, EventOwner, EventTombstone, EvidenceRelationship, FactFamily, LineRange,
    ObservationOrigin, PREDICATE_ATTRIBUTE, ServingCitation, ServingConfidence, ServingFactState,
    ServingModelError, ServingRecord, ServingResource, fact_type_may_confer_producer_authority,
    logical_repository_graph_id,
};
use ctx_attribution_index::{MAX_FLAT_RECORD_PAYLOAD_BYTES, core_source_storage_id};

const INTEGER_ATTRIBUTES: [&str; 6] = [
    "end_line_inclusive",
    "line_count_delta",
    "locator_fingerprint_revision",
    "observed_at_unix_ms",
    "origin_event_sequence",
    "start_line",
];

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("prepared Core projection is invalid: {0}")]
    Invalid(&'static str),
    #[error("prepared Core fact timestamp is invalid")]
    InvalidTimestamp,
    #[error("prepared Core projection exceeds the canonical Flat record bound")]
    FlatRecordBounds,
    #[error("prepared Core projection encoding failed")]
    Encoding,
    #[error(transparent)]
    Model(#[from] ServingModelError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectionOmissionReason {
    IneligibleProducerAuthority,
    MissingRepository,
    UnsupportedFactFamily,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectionOmission {
    pub source_id: String,
    pub event_id: String,
    pub fact_id: String,
    pub reason: ProjectionOmissionReason,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectedCoreBatch {
    pub records: Vec<ServingRecord>,
    pub owners: Vec<EventOwner>,
    pub omissions: Vec<ProjectionOmission>,
}

/// Exact bounded work performed by one page-wide Core-to-serving projection.
///
/// The serialization counters cover the canonical Flat payloads produced for
/// direct-session validation and event-output commitments. Staging and
/// publication reuse the resulting typed records; they do not revisit the
/// prepared Core facts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CorePageProjectionMetrics {
    pub page_traversals: u64,
    pub unit_traversals: u64,
    pub fact_traversals: u64,
    pub record_serializations: u64,
    pub record_serialized_bytes: u64,
}

/// Canonical evidence parallel to one projected serving record.
///
/// Fixed-width fields keep the retained page-local evidence portable and make
/// its lifetime independent of allocator layout. It is not serialized as
/// control authority; it can be reconstructed from authenticated durable
/// records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectedCoreRecordEvidence {
    pub canonical_payload_bytes: u32,
    pub canonical_sha256: [u8; 32],
    pub flat_frame_bytes: u32,
    pub index_associations: u32,
}

/// One owned page-wide projection retained only through durable staging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedCorePageProjection {
    pub projected: ProjectedCoreBatch,
    pub record_evidence: Vec<ProjectedCoreRecordEvidence>,
    pub metrics: CorePageProjectionMetrics,
}

impl PreparedCorePageProjection {
    pub fn empty() -> Self {
        Self {
            projected: ProjectedCoreBatch::default(),
            record_evidence: Vec::new(),
            metrics: CorePageProjectionMetrics {
                page_traversals: 1,
                ..CorePageProjectionMetrics::default()
            },
        }
    }
}

/// Pure Core-to-serving projection. This function performs no SQL, filesystem,
/// provider, or plaintext-spool I/O.
pub fn project_core_batch(
    prepared: &PreparedCoreProjectionBatch,
) -> Result<ProjectedCoreBatch, ProjectionError> {
    let (projected, _) = project_core_units(
        &prepared.core_generation_id,
        &prepared.source,
        prepared.units.iter(),
    )?;
    Ok(projected)
}

/// Projects one prepared Core page without cloning its facts and computes the
/// canonical Flat validation/commitment evidence exactly once.
pub fn prepare_core_page_projection(
    core_generation_id: &str,
    source: &crate::protocol::CoreSourceState,
    units: &[&PreparedCoreUnit],
) -> Result<PreparedCorePageProjection, ProjectionError> {
    let (projected, metrics) =
        project_core_units(core_generation_id, source, units.iter().copied())?;
    finalize_page_projection(projected, metrics)
}

fn finalize_page_projection(
    projected: ProjectedCoreBatch,
    mut metrics: CorePageProjectionMetrics,
) -> Result<PreparedCorePageProjection, ProjectionError> {
    let record_evidence = projected_core_record_evidence(&projected.records)?;
    metrics.record_serializations =
        u64::try_from(record_evidence.len()).map_err(|_| ProjectionError::FlatRecordBounds)?;
    metrics.record_serialized_bytes =
        record_evidence.iter().try_fold(0_u64, |total, evidence| {
            total
                .checked_add(u64::from(evidence.canonical_payload_bytes))
                .ok_or(ProjectionError::FlatRecordBounds)
        })?;
    Ok(PreparedCorePageProjection {
        projected,
        record_evidence,
        metrics,
    })
}

/// Reconstructs exact Flat evidence from authenticated durable records without
/// re-running Core fact projection.
pub fn projected_core_record_evidence(
    records: &[ServingRecord],
) -> Result<Vec<ProjectedCoreRecordEvidence>, ProjectionError> {
    records.iter().map(projected_record_evidence).collect()
}

fn project_core_units<'a>(
    core_generation_id: &str,
    source: &crate::protocol::CoreSourceState,
    units: impl Clone + Iterator<Item = &'a PreparedCoreUnit>,
) -> Result<(ProjectedCoreBatch, CorePageProjectionMetrics), ProjectionError> {
    source
        .validate()
        .map_err(|_| ProjectionError::Invalid("source state"))?;
    if !is_lower_sha256(core_generation_id) {
        return Err(ProjectionError::Invalid("Core generation ID"));
    }

    let source_id = core_source_storage_id(&source.source);
    let stable_entities = collect_stable_entities(units.clone())?;
    let mut projected = ProjectedCoreBatch::default();
    let mut owner_keys = BTreeSet::new();
    let mut metrics = CorePageProjectionMetrics {
        page_traversals: 1,
        ..CorePageProjectionMetrics::default()
    };

    for unit in units {
        metrics.unit_traversals = metrics.unit_traversals.saturating_add(1);
        let Some(owner) = event_owner(core_generation_id, source, unit)? else {
            continue;
        };
        if !owner_keys.insert(owner.key()) {
            return Err(ProjectionError::Invalid("duplicate event owner"));
        }
        for fact in &unit.facts {
            metrics.fact_traversals = metrics.fact_traversals.saturating_add(1);
            if fact.direct_actor_session_id != owner.direct_session_id
                || fact.root_session_id != owner.root_session_id
            {
                return Err(ProjectionError::Invalid("fact event ownership"));
            }
            let Some(family) = shipping_family(&fact.fact_type)? else {
                projected.omissions.push(omission(
                    &source_id,
                    unit,
                    fact,
                    ProjectionOmissionReason::UnsupportedFactFamily,
                ));
                continue;
            };
            if !unit
                .producer_authority_disposition
                .permits_positive_authority()
                && fact_type_may_confer_producer_authority(family.as_str())
            {
                projected.omissions.push(omission(
                    &source_id,
                    unit,
                    fact,
                    ProjectionOmissionReason::IneligibleProducerAuthority,
                ));
                continue;
            }
            let Some(repository_id) = fact_repository(fact)? else {
                projected.omissions.push(omission(
                    &source_id,
                    unit,
                    fact,
                    ProjectionOmissionReason::MissingRepository,
                ));
                continue;
            };
            projected.records.push(project_fact(
                source,
                unit,
                fact,
                family,
                &repository_id,
                &owner,
                &stable_entities,
            )?);
        }
        projected.owners.push(owner);
    }

    projected.records.sort_by(|left, right| {
        (
            left.event_owner.event_sequence,
            left.event_owner.event_id.as_str(),
            left.record_id.as_str(),
            &left.citations,
        )
            .cmp(&(
                right.event_owner.event_sequence,
                right.event_owner.event_id.as_str(),
                right.record_id.as_str(),
                &right.citations,
            ))
    });
    projected.owners.sort();
    projected.omissions.sort();
    Ok((projected, metrics))
}

/// Converts persisted event ownership into a replacement/deletion tombstone.
/// Requiring the prior owner prevents the adapter from inventing chronology.
pub fn project_tombstone(owner: &EventOwner) -> Result<EventTombstone, ProjectionError> {
    owner.validate()?;
    let tombstone = owner.tombstone();
    tombstone.validate()?;
    Ok(tombstone)
}

fn collect_stable_entities<'a>(
    units: impl Iterator<Item = &'a PreparedCoreUnit>,
) -> Result<BTreeMap<String, StableEntityId>, ProjectionError> {
    let mut stable_entities = BTreeMap::new();
    for identity in units.flat_map(|unit| &unit.stable_entities) {
        identity
            .validate_contract()
            .map_err(|_| ProjectionError::Invalid("stable entity identity"))?;
        let canonical = identity.to_string();
        if stable_entities
            .insert(canonical, *identity)
            .is_some_and(|prior| prior != *identity)
        {
            return Err(ProjectionError::Invalid("stable entity conflict"));
        }
    }
    Ok(stable_entities)
}

fn event_owner(
    core_generation_id: &str,
    source: &crate::protocol::CoreSourceState,
    unit: &PreparedCoreUnit,
) -> Result<Option<EventOwner>, ProjectionError> {
    if unit.facts.is_empty() {
        if unit.evidence.is_some() {
            return Err(ProjectionError::Invalid("evidence without facts"));
        }
        return Ok(None);
    }
    let evidence = unit
        .evidence
        .as_ref()
        .ok_or(ProjectionError::Invalid("facts without exact evidence"))?;
    let citation = &evidence.citation;
    if !citation.is_usable()
        || citation.core_generation_id != core_generation_id
        || !citation.source.exact_descriptor_eq(&source.source)
        || citation.event_id.to_string() != unit.origin_event_id
        || citation.evidence_sha256.is_none()
    {
        return Err(ProjectionError::Invalid("event-owned Core evidence"));
    }
    let first = unit
        .facts
        .first()
        .ok_or(ProjectionError::Invalid("missing owner fact"))?;
    if citation.session_id.to_string() != first.direct_actor_session_id {
        return Err(ProjectionError::Invalid("fact event ownership"));
    }
    let owner = EventOwner {
        source_id: core_source_storage_id(&source.source),
        event_id: unit.origin_event_id.clone(),
        direct_session_id: first.direct_actor_session_id.clone(),
        root_session_id: first.root_session_id.clone(),
        event_sequence: citation.event_sequence,
    };
    owner.validate()?;
    Ok(Some(owner))
}

#[allow(clippy::too_many_arguments)]
fn project_fact(
    source: &crate::protocol::CoreSourceState,
    unit: &PreparedCoreUnit,
    fact: &Fact,
    family: FactFamily,
    repository_id: &str,
    owner: &EventOwner,
    stable_entities: &BTreeMap<String, StableEntityId>,
) -> Result<ServingRecord, ProjectionError> {
    let subject = project_resource(
        &fact.subject,
        repository_id,
        stable_entities,
        &source.source,
    )?;
    let object = fact
        .object
        .as_ref()
        .map(|resource| project_resource(resource, repository_id, stable_entities, &source.source))
        .transpose()?;
    let scope = fact
        .root_session_id
        .as_ref()
        .map(|root| {
            project_resource(
                &ResourceRef::new(ResourceKind::Run, root),
                repository_id,
                stable_entities,
                &source.source,
            )
        })
        .transpose()?;
    let direct_actor = project_resource(
        &ResourceRef::new(ResourceKind::Session, &fact.direct_actor_session_id),
        repository_id,
        stable_entities,
        &source.source,
    )?;
    let occurred_at_unix_ms = fact
        .occurred_at
        .as_deref()
        .map(str::parse::<i64>)
        .transpose()
        .map_err(|_| ProjectionError::InvalidTimestamp)?;
    // Core's logical fact ID is the only serialized identity that retains its
    // private operation discriminator across direct projected-page staging.
    // Event evidence is excluded upstream, so equal logical operations observed
    // by different event owners still converge on this same durable ID.
    let record_id = fact.fact_id.clone();
    let evidence = unit
        .evidence
        .as_ref()
        .ok_or(ProjectionError::Invalid("facts without exact evidence"))?;
    let evidence_id = GraphRecordId::from_parts(
        "core_evidence",
        [owner.source_id.as_bytes(), unit.origin_event_id.as_bytes()],
    )
    .to_string();
    let citation = ServingCitation::from_core(
        evidence_id,
        &evidence.citation,
        &source.core_record_accumulator,
        EvidenceRelationship::Supports,
    )?;
    let mut resources = vec![&subject, &direct_actor];
    resources.extend(scope.iter());
    resources.extend(object.iter());
    let line_range = line_range(&fact.attributes)?;
    let origin = observation_origin(&fact.attributes)?;
    let confidence = fact_confidence(fact.confidence);
    let state = serving_fact_state(fact.state);
    let index_terms = index_terms(repository_id, &resources, &record_id)?;
    let attributes = typed_attributes(fact)?;
    let mut record = ServingRecord {
        record_id,
        event_owner: owner.clone(),
        repository_id: repository_id.to_owned(),
        fact_family: family,
        subject,
        object,
        scope,
        direct_actor: Some(direct_actor),
        occurred_at_unix_ms,
        confidence,
        state,
        detector_id: fact.detector_id.clone(),
        detector_revision: fact.detector_version.clone(),
        origin,
        line_range,
        index_terms,
        attributes,
        citations: vec![citation],
    };
    if record
        .fact_family
        .authority()
        .boundary()
        .is_some_and(|boundary| !record.grants_authority(boundary))
    {
        record.state = demote_asserted_authority(record.state);
    }
    if let Some(operation_id) = record.verified_commit_operation_id().map(str::to_owned) {
        record.index_terms.push(operation_id);
        record.index_terms.sort();
        record.index_terms.dedup();
    }
    record.validate_projected()?;
    record
        .to_query_fact()
        .map_err(|_| ServingModelError::InvalidText {
            field: "query fact",
        })?;
    Ok(record)
}

fn projected_record_evidence(
    record: &ServingRecord,
) -> Result<ProjectedCoreRecordEvidence, ProjectionError> {
    record.validate()?;
    let mut writer = BoundedRecordDigestWriter::default();
    if serde_json::to_writer(&mut writer, record).is_err() {
        return if writer.exceeded_bound {
            Err(ProjectionError::FlatRecordBounds)
        } else {
            Err(ProjectionError::Encoding)
        };
    }
    let canonical_payload_bytes =
        u32::try_from(writer.bytes).map_err(|_| ProjectionError::FlatRecordBounds)?;
    let flat_frame_bytes = canonical_payload_bytes
        .checked_add(4)
        .ok_or(ProjectionError::FlatRecordBounds)?;
    let index_associations = record
        .index_terms
        .len()
        .checked_mul(2)
        .and_then(|count| u32::try_from(count).ok())
        .ok_or(ProjectionError::FlatRecordBounds)?;
    Ok(ProjectedCoreRecordEvidence {
        canonical_payload_bytes,
        canonical_sha256: writer.digest.finalize().into(),
        flat_frame_bytes,
        index_associations,
    })
}

#[derive(Default)]
struct BoundedRecordDigestWriter {
    digest: Sha256,
    bytes: usize,
    exceeded_bound: bool,
}

impl io::Write for BoundedRecordDigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("canonical Flat record size overflow"))?;
        if next > MAX_FLAT_RECORD_PAYLOAD_BYTES {
            self.exceeded_bound = true;
            return Err(io::Error::other("canonical Flat record exceeds bound"));
        }
        self.bytes = next;
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn project_resource(
    resource: &ResourceRef,
    repository_id: &str,
    stable_entities: &BTreeMap<String, StableEntityId>,
    source: &crate::protocol::SourceKey,
) -> Result<ServingResource, ProjectionError> {
    if resource
        .repository_id
        .as_deref()
        .is_some_and(|resource_repository| resource_repository != repository_id)
    {
        return Err(ProjectionError::Invalid("resource repository conflict"));
    }
    let stable_entity = stable_entities.get(&resource.id).copied();
    ServingResource::from_core(
        resource.kind,
        &resource.id,
        resource.repository_id.clone(),
        resource.worktree_id.clone(),
        stable_entity,
        source,
    )
    .map_err(ProjectionError::from)
}

fn fact_repository(fact: &Fact) -> Result<Option<String>, ProjectionError> {
    let mut repositories = fact
        .object
        .iter()
        .chain(std::iter::once(&fact.subject))
        .filter_map(|resource| resource.repository_id.as_ref())
        .cloned()
        .collect::<BTreeSet<_>>();
    match repositories.len() {
        0 => Ok(None),
        1 => Ok(repositories.pop_first()),
        _ => Err(ProjectionError::Invalid("fact repository conflict")),
    }
}

fn shipping_family(value: &str) -> Result<Option<FactFamily>, ProjectionError> {
    if is_serving_fact_family(value) {
        FactFamily::new(value.to_owned())
            .map(Some)
            .map_err(ProjectionError::from)
    } else {
        Ok(None)
    }
}

fn index_terms(
    repository_id: &str,
    resources: &[&ServingResource],
    record_id: &str,
) -> Result<Vec<String>, ProjectionError> {
    let mut terms = BTreeSet::from([
        record_id.to_owned(),
        logical_repository_graph_id(repository_id)?,
    ]);
    for resource in resources {
        let display = resource.display()?;
        if display.len() <= crate::graph::segment::model::MAX_INDEX_TERM_BYTES
            && !display.chars().any(char::is_control)
        {
            terms.insert(display.clone());
        }
        terms.insert(resource.graph_id()?);
        if resource.typed_kind()? == ResourceKind::Commit
            && display.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            terms.insert(display.to_ascii_lowercase());
        }
    }
    Ok(terms.into_iter().collect())
}

const fn demote_asserted_authority(state: ServingFactState) -> ServingFactState {
    match state {
        ServingFactState::Asserted => ServingFactState::Ambiguous,
        ServingFactState::Ambiguous => ServingFactState::Ambiguous,
        ServingFactState::Contradicted => ServingFactState::Contradicted,
        ServingFactState::Superseded => ServingFactState::Superseded,
    }
}

fn typed_attributes(fact: &Fact) -> Result<BTreeMap<String, AttributeValue>, ProjectionError> {
    if fact.attributes.contains_key(PREDICATE_ATTRIBUTE) {
        return Err(ProjectionError::Invalid("reserved fact attribute"));
    }
    let mut attributes = fact
        .attributes
        .iter()
        .map(|(key, value)| {
            let typed = if INTEGER_ATTRIBUTES.contains(&key.as_str()) {
                value
                    .parse::<i64>()
                    .map(AttributeValue::Integer)
                    .unwrap_or_else(|_| AttributeValue::String(value.clone()))
            } else {
                AttributeValue::String(value.clone())
            };
            (key.clone(), typed)
        })
        .collect::<BTreeMap<_, _>>();
    attributes.insert(
        PREDICATE_ATTRIBUTE.to_owned(),
        AttributeValue::String(fact.predicate.clone()),
    );
    Ok(attributes)
}

fn line_range(attributes: &BTreeMap<String, String>) -> Result<Option<LineRange>, ProjectionError> {
    match (
        attributes.get("start_line"),
        attributes.get("end_line_inclusive"),
    ) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) => {
            let range = LineRange {
                start_line: start
                    .parse::<u32>()
                    .map_err(|_| ServingModelError::InvalidLineRange)?,
                end_line_inclusive: end
                    .parse::<u32>()
                    .map_err(|_| ServingModelError::InvalidLineRange)?,
            };
            if range.start_line == 0 || range.end_line_inclusive < range.start_line {
                return Err(ServingModelError::InvalidLineRange.into());
            }
            Ok(Some(range))
        }
        _ => Err(ServingModelError::InvalidLineRange.into()),
    }
}

fn observation_origin(
    attributes: &BTreeMap<String, String>,
) -> Result<ObservationOrigin, ProjectionError> {
    match attributes.get("observation_origin").map(String::as_str) {
        Some("direct") => Ok(ObservationOrigin::Direct),
        Some("copied") => {
            let origin_event_id = attributes
                .get("origin_event_id")
                .cloned()
                .ok_or(ServingModelError::InvalidOrigin)?;
            let origin_session_id = attributes
                .get("origin_session_id")
                .cloned()
                .ok_or(ServingModelError::InvalidOrigin)?;
            Ok(ObservationOrigin::Copied {
                origin_event_id,
                origin_session_id,
            })
        }
        Some("later") => Ok(ObservationOrigin::Later {
            origin_event_sequence: attributes
                .get("origin_event_sequence")
                .ok_or(ServingModelError::InvalidOrigin)?
                .parse::<u64>()
                .map_err(|_| ServingModelError::InvalidOrigin)?,
        }),
        Some(_) => Ok(ObservationOrigin::Unspecified),
        None if attributes.contains_key("outcome_capture_revision")
            && attributes.contains_key("result_record_sha256") =>
        {
            Ok(ObservationOrigin::Later {
                origin_event_sequence: attributes
                    .get("origin_event_sequence")
                    .ok_or(ServingModelError::InvalidOrigin)?
                    .parse::<u64>()
                    .map_err(|_| ServingModelError::InvalidOrigin)?,
            })
        }
        None => Ok(ObservationOrigin::Unspecified),
    }
}

fn omission(
    source_id: &str,
    unit: &PreparedCoreUnit,
    fact: &Fact,
    reason: ProjectionOmissionReason,
) -> ProjectionOmission {
    ProjectionOmission {
        source_id: source_id.to_owned(),
        event_id: unit.origin_event_id.clone(),
        fact_id: fact.fact_id.clone(),
        reason,
    }
}

const fn serving_fact_state(value: FactState) -> ServingFactState {
    match value {
        FactState::Asserted => ServingFactState::Asserted,
        FactState::Ambiguous => ServingFactState::Ambiguous,
        FactState::Contradicted => ServingFactState::Contradicted,
        FactState::Superseded => ServingFactState::Superseded,
    }
}

const fn fact_confidence(value: Confidence) -> ServingConfidence {
    match value {
        Confidence::Verified => ServingConfidence::Verified,
        Confidence::High => ServingConfidence::High,
        Confidence::Medium => ServingConfidence::Medium,
        Confidence::Ambiguous => ServingConfidence::Ambiguous,
    }
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests;

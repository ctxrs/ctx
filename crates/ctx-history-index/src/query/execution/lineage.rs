use std::collections::{HashMap, HashSet};

use ctx_history_core::{
    EventOrigin, SessionRelationshipKind, SourceKey, StableEntityId, StableEntityKind,
};
use serde::Deserialize;
use tantivy::{
    schema::IndexRecordOption, DocAddress, DocId, DocSet, SegmentReader, TantivyDocument, Term,
    TERMINATED,
};
use uuid::Uuid;

use crate::{fields_from_schema, hex, Fields, IndexError, Result, VerifiedIndex};

use super::super::{
    CopiedEventLineage, CopiedEventLineageOccurrence, CopiedEventLineagePolicy,
    CopiedEventLineageRelationshipCount, MAX_COPIED_EVENT_LINEAGE_DEPTH,
    MAX_COPIED_EVENT_LINEAGE_EXACT_IDENTITY_POSTING_VISITS,
};

#[derive(Debug, Clone)]
struct LineageEvent {
    event_id: StableEntityId,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    session_relationship: SessionRelationshipKind,
    event_origin: EventOrigin,
    event_sequence: u64,
}

impl LineageEvent {
    fn copied_from(&self) -> Option<(StableEntityId, StableEntityId)> {
        self.event_origin
            .copied_from_ancestor()
            .map(|(session_id, event_id, _)| (session_id, event_id))
    }

    fn order_key(&self) -> (Uuid, u64, Uuid) {
        (
            self.session_id.as_uuid(),
            self.event_sequence,
            self.event_id.as_uuid(),
        )
    }

    fn session_lineage(&self) -> SessionLineage {
        SessionLineage {
            parent_session_id: self.parent_session_id,
            root_session_id: self.root_session_id,
            session_relationship: self.session_relationship,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionLineage {
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    session_relationship: SessionRelationshipKind,
}

#[derive(Deserialize)]
struct StoredLineageProjection {
    event_id: StableEntityId,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    session_relationship: SessionRelationshipKind,
    event_origin: EventOrigin,
    source: SourceKey,
    event_sequence: u64,
    is_primary: bool,
}

impl VerifiedIndex {
    /// Resolves one selected event to its ultimate non-copy origin, then walks
    /// exact reverse copied-event edges breadth-first within the caller's
    /// explicit work and retention ceilings.
    ///
    /// `observed_count` and every relationship count are exact only when the
    /// returned result is not truncated. A missing selected event returns
    /// `None`; malformed stored or indexed lineage fails closed.
    pub fn copied_event_lineage(
        &self,
        selected_event_id: Uuid,
        policy: CopiedEventLineagePolicy,
    ) -> Result<Option<CopiedEventLineage>> {
        policy.validate()?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let mut exact_identity_posting_visits = 0_usize;
        let Some(selected) = self.lineage_event_by_uuid(
            selected_event_id,
            fields,
            &mut exact_identity_posting_visits,
        )?
        else {
            return Ok(None);
        };
        let (canonical, selected_depth) = self.resolve_canonical_event(
            selected.clone(),
            fields,
            &mut exact_identity_posting_visits,
        )?;

        let mut visited_events = HashSet::new();
        visited_events.insert(canonical.event_id);
        let mut visited_sessions = HashMap::new();
        visited_sessions.insert(canonical.session_id, canonical.session_lineage());
        let mut frontier = vec![canonical.clone()];
        let mut frontier_depth = 0_usize;
        let mut posting_visits = 0_usize;
        let mut observed_count = 0_u64;
        let mut relationship_counts = [0_u64; 6];
        let mut occurrences = Vec::with_capacity(policy.maximum_occurrences);
        let mut truncated = false;

        loop {
            let child_depth = frontier_depth
                .checked_add(1)
                .ok_or(IndexError::CountOverflow)?;
            let mut children = Vec::new();
            let mut inverse_complete = true;
            for ancestor in &frontier {
                if !self.inverse_lineage_children(
                    ancestor,
                    fields,
                    policy.maximum_posting_visits,
                    &mut posting_visits,
                    &mut children,
                )? {
                    inverse_complete = false;
                    truncated = true;
                    break;
                }
            }
            children.sort_by_key(LineageEvent::order_key);

            if child_depth > MAX_COPIED_EVENT_LINEAGE_DEPTH {
                if !children.is_empty() || !inverse_complete {
                    truncated = true;
                }
                break;
            }

            let mut next_frontier = Vec::with_capacity(children.len());
            for child in children {
                if !visited_events.insert(child.event_id) {
                    return Err(IndexError::InvalidEventOriginGraph(
                        "cycle or duplicate event in reverse copied-event lineage",
                    ));
                }

                let session_lineage = child.session_lineage();
                match visited_sessions.get(&child.session_id) {
                    Some(existing) if *existing != session_lineage => {
                        return Err(IndexError::InvalidSessionRelationshipGraph(
                            "one session has inconsistent stored lineage",
                        ));
                    }
                    Some(_) => {}
                    None => {
                        visited_sessions.insert(child.session_id, session_lineage);
                        observed_count = observed_count
                            .checked_add(1)
                            .ok_or(IndexError::CountOverflow)?;
                        let count = relationship_counts
                            .get_mut(relationship_index(child.session_relationship))
                            .ok_or(IndexError::CountOverflow)?;
                        *count = count.checked_add(1).ok_or(IndexError::CountOverflow)?;

                        let (copied_from_session_id, copied_from_event_id) = child
                            .copied_from()
                            .ok_or(IndexError::InvalidEventOriginGraph(
                                "inverse posting does not identify a copied event",
                            ))?;
                        if occurrences.len() < policy.maximum_occurrences {
                            occurrences.push(CopiedEventLineageOccurrence {
                                event_id: child.event_id,
                                session_id: child.session_id,
                                copied_from_event_id,
                                copied_from_session_id,
                                parent_session_id: child.parent_session_id,
                                root_session_id: child.root_session_id,
                                session_relationship: child.session_relationship,
                                depth: child_depth,
                            });
                        }
                    }
                }
                next_frontier.push(child);
            }

            if !inverse_complete || next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
            frontier_depth = child_depth;
        }

        let relationship_counts = relationship_counts
            .into_iter()
            .zip(RELATIONSHIP_ORDER)
            .filter_map(|(observed_count, session_relationship)| {
                (observed_count != 0).then_some(CopiedEventLineageRelationshipCount {
                    session_relationship,
                    observed_count,
                })
            })
            .collect::<Vec<_>>();
        let returned = occurrences.len();
        Ok(Some(CopiedEventLineage {
            generation_id: self.generation_id.clone(),
            selected_event_id: selected.event_id,
            selected_session_id: selected.session_id,
            canonical_event_id: canonical.event_id,
            canonical_session_id: canonical.session_id,
            selected_depth,
            observed_count,
            returned,
            occurrences,
            relationship_counts,
            truncated,
        }))
    }

    fn resolve_canonical_event(
        &self,
        selected: LineageEvent,
        fields: Fields,
        exact_identity_posting_visits: &mut usize,
    ) -> Result<(LineageEvent, usize)> {
        let mut current = selected;
        let mut depth = 0_usize;
        let mut visited = HashSet::new();
        visited.insert(current.event_id);
        while let Some((ancestor_session_id, ancestor_event_id)) = current.copied_from() {
            if depth == MAX_COPIED_EVENT_LINEAGE_DEPTH {
                return Err(IndexError::InvalidEventOriginGraph(
                    "copied-event origin exceeds maximum depth",
                ));
            }
            if current.session_relationship == SessionRelationshipKind::Root {
                return Err(IndexError::InvalidEventOriginGraph(
                    "a copied event cannot belong to a root session",
                ));
            }
            let ancestor = self
                .lineage_event_by_uuid(
                    ancestor_event_id.as_uuid(),
                    fields,
                    exact_identity_posting_visits,
                )?
                .ok_or(IndexError::InvalidEventOriginGraph(
                    "declared origin event does not exist",
                ))?;
            if ancestor.event_id != ancestor_event_id
                || ancestor.session_id != ancestor_session_id
                || ancestor.root_session_id != current.root_session_id
            {
                return Err(IndexError::InvalidEventOriginGraph(
                    "declared origin event identity or session is inconsistent",
                ));
            }
            if !visited.insert(ancestor.event_id) {
                return Err(IndexError::InvalidEventOriginGraph(
                    "cycle in copied-event origin graph",
                ));
            }
            current = ancestor;
            depth = depth.checked_add(1).ok_or(IndexError::CountOverflow)?;
        }
        Ok((current, depth))
    }

    fn lineage_event_by_uuid(
        &self,
        event_id: Uuid,
        fields: Fields,
        exact_identity_posting_visits: &mut usize,
    ) -> Result<Option<LineageEvent>> {
        let term = Term::from_field_text(fields.event_id, &event_id.to_string());
        let mut found = None;
        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.event_id)?;
            let Some(term_info) = inverted.get_term_info(&term)? else {
                continue;
            };
            let mut postings =
                inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                if *exact_identity_posting_visits
                    == MAX_COPIED_EVENT_LINEAGE_EXACT_IDENTITY_POSTING_VISITS
                {
                    return Err(
                        IndexError::CopiedEventLineageExactIdentityPostingVisitLimitExceeded {
                            maximum: MAX_COPIED_EVENT_LINEAGE_EXACT_IDENTITY_POSTING_VISITS,
                        },
                    );
                }
                *exact_identity_posting_visits = (*exact_identity_posting_visits)
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                if !segment.is_deleted(doc_id) {
                    if found.is_some() {
                        return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
                    }
                    let segment_ord =
                        u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                    let record = stored_lineage_event(
                        &self.searcher,
                        DocAddress::new(segment_ord, doc_id),
                        fields,
                    )?;
                    if record.event_id.as_uuid() != event_id {
                        return Err(IndexError::InvalidStoredDocumentField("event_id"));
                    }
                    found = Some(record);
                }
                doc_id = postings.advance();
            }
        }
        Ok(found)
    }

    fn inverse_lineage_children(
        &self,
        ancestor: &LineageEvent,
        fields: Fields,
        maximum_posting_visits: usize,
        posting_visits: &mut usize,
        children: &mut Vec<LineageEvent>,
    ) -> Result<bool> {
        let term = Term::from_field_text(
            fields.origin_event_identity_digest,
            &hex(&ancestor.event_id.digest()),
        );
        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.origin_event_identity_digest)?;
            let Some(term_info) = inverted.get_term_info(&term)? else {
                continue;
            };
            let mut postings =
                inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                if *posting_visits == maximum_posting_visits {
                    return Ok(false);
                }
                *posting_visits = (*posting_visits)
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                if !segment.is_deleted(doc_id) {
                    let segment_ord =
                        u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                    let child = stored_lineage_event(
                        &self.searcher,
                        DocAddress::new(segment_ord, doc_id),
                        fields,
                    )?;
                    let Some((copied_from_session_id, copied_from_event_id)) = child.copied_from()
                    else {
                        return Err(IndexError::InvalidEventOriginGraph(
                            "inverse posting does not identify a copied event",
                        ));
                    };
                    if copied_from_event_id != ancestor.event_id
                        || copied_from_session_id != ancestor.session_id
                        || child.root_session_id != ancestor.root_session_id
                    {
                        return Err(IndexError::InvalidEventOriginGraph(
                            "inverse posting disagrees with the exact stored origin",
                        ));
                    }
                    if child.session_relationship == SessionRelationshipKind::Root {
                        return Err(IndexError::InvalidEventOriginGraph(
                            "a copied event cannot belong to a root session",
                        ));
                    }
                    children.push(child);
                }
                doc_id = postings.advance();
            }
        }
        Ok(true)
    }
}

fn stored_lineage_event(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<LineageEvent> {
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded =
        super::super::records::unique_required_bytes(&document, fields.core_record, "core_record")?;
    super::super::records::validate_core_record_encoded_bytes(searcher, address, encoded.len())?;
    let projection: StoredLineageProjection = serde_json::from_slice(encoded)?;
    validate_stored_lineage_projection(&projection)?;
    validate_indexed_lineage_projection(searcher, address, fields, &projection)?;
    Ok(LineageEvent {
        event_id: projection.event_id,
        session_id: projection.session_id,
        parent_session_id: projection.parent_session_id,
        root_session_id: projection.root_session_id,
        session_relationship: projection.session_relationship,
        event_origin: projection.event_origin,
        event_sequence: projection.event_sequence,
    })
}

fn validate_stored_lineage_projection(projection: &StoredLineageProjection) -> Result<()> {
    projection.source.validate_contract()?;
    validate_owned_identity(
        projection.event_id,
        StableEntityKind::Event,
        &projection.source,
    )?;
    validate_owned_identity(
        projection.session_id,
        StableEntityKind::Session,
        &projection.source,
    )?;
    validate_related_session_identity(projection.root_session_id)?;
    if let Some(parent_session_id) = projection.parent_session_id {
        validate_related_session_identity(parent_session_id)?;
    }
    if projection.is_primary != projection.session_relationship.is_primary() {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    match projection.session_relationship {
        SessionRelationshipKind::Root => {
            if projection.parent_session_id.is_some()
                || projection.root_session_id != projection.session_id
            {
                return Err(IndexError::InvalidStoredDocumentField("core_record"));
            }
        }
        SessionRelationshipKind::Delegated
        | SessionRelationshipKind::Forked
        | SessionRelationshipKind::ResumedFrom
        | SessionRelationshipKind::WorkflowChild
        | SessionRelationshipKind::RelatedUnknown => {
            let Some(parent_session_id) = projection.parent_session_id else {
                return Err(IndexError::InvalidStoredDocumentField("core_record"));
            };
            if parent_session_id == projection.session_id
                || projection.root_session_id == projection.session_id
            {
                return Err(IndexError::InvalidStoredDocumentField("core_record"));
            }
        }
    }
    if let Some((ancestor_session_id, ancestor_event_id, _)) =
        projection.event_origin.copied_from_ancestor()
    {
        validate_related_session_identity(ancestor_session_id)?;
        ancestor_event_id.validate_contract()?;
        if ancestor_event_id.entity_kind() != StableEntityKind::Event
            || ancestor_session_id == projection.session_id
            || ancestor_event_id == projection.event_id
        {
            return Err(IndexError::InvalidStoredDocumentField("core_record"));
        }
    }
    Ok(())
}

fn validate_owned_identity(
    identity: StableEntityId,
    expected_kind: StableEntityKind,
    source: &SourceKey,
) -> Result<()> {
    identity.validate_contract()?;
    if identity.entity_kind() != expected_kind
        || identity.source_digest() != source.identity().digest()
        || identity.source_descriptor_digest() != source.exact_descriptor_digest()
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    Ok(())
}

fn validate_related_session_identity(identity: StableEntityId) -> Result<()> {
    identity.validate_contract()?;
    if identity.entity_kind() != StableEntityKind::Session {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    Ok(())
}

fn validate_indexed_lineage_projection(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
    projection: &StoredLineageProjection,
) -> Result<()> {
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField("core_record"))?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.event_id,
        &projection.event_id.to_string(),
        "event_id",
    )?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.event_identity_digest,
        &hex(&projection.event_id.digest()),
        "event_identity_digest",
    )?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.session_id,
        &projection.session_id.to_string(),
        "session_id",
    )?;
    if let Some(parent_session_id) = projection.parent_session_id {
        validate_text_posting(
            segment,
            address.doc_id,
            fields.parent_session_id,
            &parent_session_id.to_string(),
            "parent_session_id",
        )?;
    }
    validate_text_posting(
        segment,
        address.doc_id,
        fields.root_session_id,
        &projection.root_session_id.to_string(),
        "root_session_id",
    )?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.session_relationship_kind,
        projection.session_relationship.as_str(),
        "session_relationship_kind",
    )?;
    validate_text_posting(
        segment,
        address.doc_id,
        fields.event_origin_kind,
        projection.event_origin.kind_str(),
        "event_origin_kind",
    )?;
    if let Some((_, ancestor_event_id, _)) = projection.event_origin.copied_from_ancestor() {
        validate_text_posting(
            segment,
            address.doc_id,
            fields.origin_event_identity_digest,
            &hex(&ancestor_event_id.digest()),
            "origin_event_identity_digest",
        )?;
    }
    validate_u64_posting(
        segment,
        address.doc_id,
        fields.is_primary,
        u64::from(projection.is_primary),
        "is_primary",
    )?;
    validate_u64_posting(
        segment,
        address.doc_id,
        fields.event_sequence,
        projection.event_sequence,
        "event_sequence",
    )?;

    let event = projection.event_id.as_uuid().as_u128();
    validate_fast_u64(
        segment,
        address.doc_id,
        "event_id_high",
        (event >> 64) as u64,
    )?;
    validate_fast_u64(segment, address.doc_id, "event_id_low", event as u64)?;
    let session = projection.session_id.as_uuid().as_u128();
    validate_fast_u64(
        segment,
        address.doc_id,
        "session_id_high",
        (session >> 64) as u64,
    )?;
    validate_fast_u64(segment, address.doc_id, "session_id_low", session as u64)?;
    validate_fast_u64(
        segment,
        address.doc_id,
        "event_sequence",
        projection.event_sequence,
    )?;
    Ok(())
}

fn validate_text_posting(
    segment: &SegmentReader,
    doc_id: DocId,
    field: tantivy::schema::Field,
    expected: &str,
    field_name: &'static str,
) -> Result<()> {
    validate_term_posting(
        segment,
        doc_id,
        field,
        Term::from_field_text(field, expected),
        field_name,
    )
}

fn validate_u64_posting(
    segment: &SegmentReader,
    doc_id: DocId,
    field: tantivy::schema::Field,
    expected: u64,
    field_name: &'static str,
) -> Result<()> {
    validate_term_posting(
        segment,
        doc_id,
        field,
        Term::from_field_u64(field, expected),
        field_name,
    )
}

fn validate_term_posting(
    segment: &SegmentReader,
    doc_id: DocId,
    field: tantivy::schema::Field,
    term: Term,
    field_name: &'static str,
) -> Result<()> {
    let inverted = segment.inverted_index(field)?;
    let term_info = inverted
        .get_term_info(&term)?
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    let mut postings =
        inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
    if postings.seek(doc_id) != doc_id {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(())
}

fn validate_fast_u64(
    segment: &SegmentReader,
    doc_id: DocId,
    field_name: &'static str,
    expected: u64,
) -> Result<()> {
    let column = segment.fast_fields().u64(field_name)?;
    let mut values = column.values_for_doc(doc_id);
    if values.next() != Some(expected) || values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(())
}

fn relationship_index(relationship: SessionRelationshipKind) -> usize {
    match relationship {
        SessionRelationshipKind::Root => 0,
        SessionRelationshipKind::Delegated => 1,
        SessionRelationshipKind::Forked => 2,
        SessionRelationshipKind::ResumedFrom => 3,
        SessionRelationshipKind::WorkflowChild => 4,
        SessionRelationshipKind::RelatedUnknown => 5,
    }
}

const RELATIONSHIP_ORDER: [SessionRelationshipKind; 6] = [
    SessionRelationshipKind::Root,
    SessionRelationshipKind::Delegated,
    SessionRelationshipKind::Forked,
    SessionRelationshipKind::ResumedFrom,
    SessionRelationshipKind::WorkflowChild,
    SessionRelationshipKind::RelatedUnknown,
];

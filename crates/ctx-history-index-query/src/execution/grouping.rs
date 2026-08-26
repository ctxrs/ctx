use super::*;

use ctx_history_index_format::SessionAuthorityKey;

const MAX_SESSION_GROUPING_CLAIM_RECORDS: usize = 4_097;
/// Bounds the fixed segment fan-out before any authority dictionary is opened.
///
/// This matches the manual lexical executor's segment ceiling.
const MAX_SESSION_GROUPING_SEGMENT_PROBES: usize = 512;
/// Bounds the complete `(segment, compact session coordinate)` cross-product.
const MAX_SESSION_GROUPING_TERM_SEEKS: usize =
    MAX_SESSION_GROUPING_COORDINATES * MAX_SESSION_GROUPING_SEGMENT_PROBES;
/// Includes deleted documents, so a delete-heavy posting list cannot evade it;
/// the cap is twice the maximum possible live witness set.
const MAX_SESSION_GROUPING_POSTING_VISITS: usize = 65_536;
#[derive(Debug)]
struct SessionGroupingWorkMeter {
    segment_probes: usize,
    term_seeks: usize,
    dictionary_terms: usize,
    posting_visits: usize,
    live_witnesses: usize,
}

impl SessionGroupingWorkMeter {
    fn charge(
        counter: &mut usize,
        amount: usize,
        maximum: usize,
        operation: &'static str,
    ) -> Result<()> {
        let next = counter
            .checked_add(amount)
            .ok_or(IndexError::SessionGroupingAuthorityWorkLimitExceeded { operation, maximum })?;
        if next > maximum {
            return Err(IndexError::SessionGroupingAuthorityWorkLimitExceeded {
                operation,
                maximum,
            });
        }
        *counter = next;
        Ok(())
    }

    fn segment_probes(&mut self, amount: usize) -> Result<()> {
        Self::charge(
            &mut self.segment_probes,
            amount,
            MAX_SESSION_GROUPING_SEGMENT_PROBES,
            "segment probes",
        )
    }

    fn term_seeks(&mut self, amount: usize) -> Result<()> {
        Self::charge(
            &mut self.term_seeks,
            amount,
            MAX_SESSION_GROUPING_TERM_SEEKS,
            "exact authority term seeks",
        )
    }

    fn dictionary_term(&mut self) -> Result<()> {
        Self::charge(
            &mut self.dictionary_terms,
            1,
            MAX_SESSION_GROUPING_TERM_SEEKS,
            "authority dictionary terms",
        )
    }

    fn posting_visit(&mut self) -> Result<()> {
        Self::charge(
            &mut self.posting_visits,
            1,
            MAX_SESSION_GROUPING_POSTING_VISITS,
            "authority posting visits",
        )
    }

    fn live_witness(&mut self) -> Result<()> {
        Self::charge(
            &mut self.live_witnesses,
            1,
            MAX_SESSION_GROUPING_WITNESSES,
            "live authority witnesses",
        )
    }
}

fn after_segment_preflight<T>(
    meter: &mut SessionGroupingWorkMeter,
    segment_count: usize,
    admitted_work: impl FnOnce() -> T,
) -> Result<T> {
    meter.segment_probes(segment_count)?;
    Ok(admitted_work())
}

#[derive(Debug)]
struct GroupingAccumulator {
    claims: SessionGroupingClaims,
    witnesses: usize,
}

impl VerifiedIndex {
    /// Coalesces the sparse live authority witnesses for unique exact session
    /// coordinates through an explicit bounded exact-term authority scan.
    ///
    /// Results preserve request order. Missing coordinates, duplicate input,
    /// unexpected authority, duplicate events, more than four witnesses for a
    /// coordinate, and conflicting positive claims fail the complete batch.
    pub fn session_grouping_claims(
        &self,
        coordinates: &[(StableEntityId, StableEntityId)],
    ) -> Result<Vec<SessionGroupingClaims>> {
        if coordinates.len() > MAX_SESSION_GROUPING_COORDINATES {
            return Err(IndexError::InvalidSessionGroupingCoordinateCount {
                requested: coordinates.len(),
                maximum: MAX_SESSION_GROUPING_COORDINATES,
            });
        }
        let mut requested = BTreeSet::new();
        let mut compact = Vec::with_capacity(coordinates.len());
        for &(session_id, source_owner) in coordinates {
            let key = SessionAuthorityKey::exact(session_id, source_owner)?;
            if !requested.insert(key) {
                return Err(IndexError::DuplicateSessionGroupingCoordinate(format!(
                    "{session_id}@{source_owner}"
                )));
            }
            compact.push(SearchSessionCoordinate {
                session_id: session_id.as_uuid(),
                source_owner_digest: source_owner.digest(),
            });
        }
        let claims = self.session_grouping_claims_for_search(&compact)?;
        if claims.len() != coordinates.len()
            || claims
                .iter()
                .zip(coordinates)
                .any(|(claims, &(session_id, source_owner))| {
                    claims.session_id != session_id || claims.source_owner != source_owner
                })
        {
            return Err(IndexError::InvalidStoredDocumentField("session_authority"));
        }
        Ok(claims)
    }

    /// Resolves compact candidate coordinates to their exact sparse session
    /// authority and coalesces all live witnesses in the same bounded scan.
    /// A verified generation guarantees that a compact session UUID names at
    /// most one full session identity; the full source digest prevents a
    /// candidate from crossing source ownership while resolving that UUID.
    pub fn session_grouping_claims_for_search(
        &self,
        coordinates: &[SearchSessionCoordinate],
    ) -> Result<Vec<SessionGroupingClaims>> {
        if coordinates.len() > MAX_SESSION_GROUPING_COORDINATES {
            return Err(IndexError::InvalidSessionGroupingCoordinateCount {
                requested: coordinates.len(),
                maximum: MAX_SESSION_GROUPING_COORDINATES,
            });
        }
        if coordinates.is_empty() {
            return Ok(Vec::new());
        }

        let fields = fields_from_schema(self.searcher.schema())?;
        let mut requested = BTreeSet::new();
        for coordinate in coordinates.iter().copied() {
            if !requested.insert(coordinate) {
                return Err(IndexError::DuplicateSessionGroupingCoordinate(format!(
                    "{}@{}",
                    coordinate.session_id,
                    hex(&coordinate.source_owner_digest),
                )));
            }
        }

        let mut meter = SessionGroupingWorkMeter {
            segment_probes: 0,
            term_seeks: 0,
            dictionary_terms: 0,
            posting_visits: 0,
            live_witnesses: 0,
        };
        let segment_readers = self.searcher.segment_readers();
        // Reject an over-segmented generation before allocating or sorting a
        // request-sized segment list, and before opening an authority
        // dictionary.
        let mut stable_segments =
            after_segment_preflight(&mut meter, segment_readers.len(), || {
                segment_readers.iter().enumerate().collect::<Vec<_>>()
            })?;
        stable_segments.sort_by_key(|(_, segment)| segment.segment_id());
        let term_seek_count = stable_segments
            .len()
            .checked_mul(requested.len())
            .ok_or(IndexError::CountOverflow)?;
        // Admit the complete cross-product before opening an authority
        // dictionary, rather than allowing a large request to make partial
        // unmetered progress across segments.
        meter.term_seeks(term_seek_count)?;

        let mut grouped = BTreeMap::<SearchSessionCoordinate, GroupingAccumulator>::new();
        for (_, segment) in stable_segments {
            let inverted = segment.inverted_index(fields.session_authority)?;
            for coordinate in &requested {
                let prefix = SessionAuthorityKey::uuid_prefix_from_uuid(coordinate.session_id);
                let range_end =
                    SessionAuthorityKey::uuid_range_end_from_uuid(coordinate.session_id);
                let mut terms = inverted
                    .terms()
                    .range()
                    .ge(prefix)
                    .lt(&range_end)
                    .into_stream()?;
                while terms.advance() {
                    meter.dictionary_term()?;
                    let key = SessionAuthorityKey::decode(terms.key())?;
                    let (session_id, source_owner) = key.identities()?;
                    if session_id.as_uuid() != coordinate.session_id
                        || source_owner.digest() != coordinate.source_owner_digest
                    {
                        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
                    }
                    let direct = key.direct_claims()?;
                    let candidate = SessionGroupingClaims {
                        session_id,
                        source_owner,
                        parent_session_id: direct.parent_session_id,
                        root_session_id: direct.root_session_id,
                        relationship: direct.relationship,
                    };
                    let mut postings = inverted
                        .read_postings_from_terminfo(terms.value(), IndexRecordOption::Basic)?;
                    let mut doc_id = postings.doc();
                    while doc_id != TERMINATED {
                        meter.posting_visit()?;
                        if !segment.is_deleted(doc_id) {
                            meter.live_witness()?;
                            match grouped.entry(*coordinate) {
                                std::collections::btree_map::Entry::Vacant(entry) => {
                                    entry.insert(GroupingAccumulator {
                                        claims: candidate,
                                        witnesses: 1,
                                    });
                                }
                                std::collections::btree_map::Entry::Occupied(mut entry) => {
                                    let accumulator = entry.get_mut();
                                    accumulator.witnesses = accumulator
                                        .witnesses
                                        .checked_add(1)
                                        .ok_or(IndexError::CountOverflow)?;
                                    if accumulator.witnesses
                                        > MAX_SESSION_GROUPING_WITNESSES_PER_COORDINATE
                                    {
                                        return Err(IndexError::InvalidStoredDocumentField(
                                            "session_authority",
                                        ));
                                    }
                                    accumulator.claims =
                                        merge_grouping_claims(accumulator.claims, candidate)?;
                                }
                            }
                        }
                        doc_id = postings.advance();
                    }
                }
            }
        }

        let mut ordered = Vec::with_capacity(coordinates.len());
        for coordinate in coordinates {
            let Some(accumulator) = grouped.remove(coordinate) else {
                return Err(IndexError::MissingSessionGroupingCoordinate(format!(
                    "{}@{}",
                    coordinate.session_id,
                    hex(&coordinate.source_owner_digest),
                )));
            };
            if !(1..=MAX_SESSION_GROUPING_WITNESSES_PER_COORDINATE).contains(&accumulator.witnesses)
            {
                return Err(IndexError::InvalidStoredDocumentField("session_authority"));
            }
            if accumulator.claims.session_id.as_uuid() != coordinate.session_id
                || accumulator.claims.source_owner.digest() != coordinate.source_owner_digest
            {
                return Err(IndexError::InvalidStoredDocumentField("session_authority"));
            }
            ordered.push(accumulator.claims);
        }
        if !grouped.is_empty() {
            return Err(IndexError::InvalidStoredDocumentField("session_authority"));
        }
        Ok(ordered)
    }

    /// Resolves one compact session ID, then reads its exact grouping
    /// authority. This is retained for the active-session safety exception.
    pub fn session_grouping_claims_by_id(
        &self,
        session_id: Uuid,
    ) -> Result<Option<SessionGroupingClaims>> {
        let Some(coordinate) = self
            .session_event_coordinate_prefix(session_id, 1)?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let event = self
            .event_by_id(coordinate.event_id)?
            .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?;
        if event.session_id.as_uuid() != session_id {
            return Err(IndexError::InvalidStoredDocumentField("session_id"));
        }
        let mut claims =
            self.session_grouping_claims(&[(event.session_id, event.source.identity())])?;
        Ok(claims.pop())
    }

    /// Returns bounded grouping claims whose indexed direct parent or root
    /// claim names any supplied session ID.
    pub fn session_grouping_claims_claiming_lineage_to_any(
        &self,
        session_ids: &[Uuid],
        limit: usize,
    ) -> Result<Vec<SessionGroupingClaims>> {
        if session_ids.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if limit > MAX_SESSION_GROUPING_CLAIM_RECORDS {
            return Err(IndexError::InvalidStoredDocumentField("session_authority"));
        }
        let fields = fields_from_schema(self.searcher.schema())?;
        let queries = [fields.parent_session_id, fields.root_session_id]
            .into_iter()
            .map(|field| {
                Box::new(TermSetQuery::new(
                    session_ids
                        .iter()
                        .map(|session_id| Term::from_field_text(field, &session_id.to_string()))
                        .collect::<Vec<_>>(),
                )) as Box<dyn Query>
            })
            .collect();
        let candidate_ids = self.searcher.search(
            &BooleanQuery::union(queries),
            &SessionIdCollector::new(limit),
        )?;
        let mut candidates = Vec::with_capacity(candidate_ids.len());
        for session_id in candidate_ids {
            let Some(candidate) = self.session_grouping_claims_by_id(session_id)? else {
                return Err(IndexError::InvalidStoredDocumentField("session_authority"));
            };
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}

fn merge_grouping_claims(
    existing: SessionGroupingClaims,
    candidate: SessionGroupingClaims,
) -> Result<SessionGroupingClaims> {
    if existing.session_id != candidate.session_id
        || existing.source_owner != candidate.source_owner
    {
        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
    }
    Ok(SessionGroupingClaims {
        session_id: existing.session_id,
        source_owner: existing.source_owner,
        parent_session_id: merge_direct_claim(
            existing.parent_session_id,
            candidate.parent_session_id,
        )?,
        root_session_id: merge_direct_claim(existing.root_session_id, candidate.root_session_id)?,
        relationship: merge_direct_claim(existing.relationship, candidate.relationship)?,
    })
}

fn merge_direct_claim<T: Copy + Eq>(left: Option<T>, right: Option<T>) -> Result<Option<T>> {
    match (left, right) {
        (Some(left), Some(right)) if left == right => Ok(Some(left)),
        (Some(_), Some(_)) => Err(IndexError::ConflictingProviderNativeSessionClaim(
            "one session has contradictory relationship fields",
        )),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_preflight_rejects_before_running_admitted_work() {
        let admitted_work_ran = std::cell::Cell::new(false);
        let mut meter = SessionGroupingWorkMeter {
            segment_probes: 0,
            term_seeks: 0,
            dictionary_terms: 0,
            posting_visits: 0,
            live_witnesses: 0,
        };

        assert!(matches!(
            after_segment_preflight(&mut meter, MAX_SESSION_GROUPING_SEGMENT_PROBES + 1, || {
                admitted_work_ran.set(true)
            },),
            Err(IndexError::SessionGroupingAuthorityWorkLimitExceeded {
                operation: "segment probes",
                maximum: MAX_SESSION_GROUPING_SEGMENT_PROBES,
            })
        ));
        assert_eq!(meter.segment_probes, 0);
        assert!(!admitted_work_ran.get());
    }
}

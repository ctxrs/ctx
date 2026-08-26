use super::*;

pub(super) fn relationship_kind_tag(
    kind: Option<ctx_history_core::ProviderNativeSessionRelationship>,
) -> Option<u8> {
    use ctx_history_core::ProviderNativeSessionRelationship as Relationship;
    match kind {
        None => None,
        Some(Relationship::Root) => Some(1),
        Some(Relationship::Delegated) => Some(2),
        Some(Relationship::Forked) => Some(3),
        Some(Relationship::ResumedFrom) => Some(4),
        Some(Relationship::WorkflowChild) => Some(5),
    }
}

pub(super) fn verify_session_witness_key(record: &VerificationRecord) -> Result<bool> {
    let Some(key) = record.session_authority else {
        return Ok(false);
    };
    let key = SessionAuthorityKey::decode(&key)?;
    if key != SessionAuthorityKey::for_core_record(&record.core_record)? {
        return Err(IndexError::InvalidStoredDocumentField("session_authority"));
    }
    Ok(true)
}

pub(super) fn verify_event_identities(
    searcher: &Searcher,
    field: Field,
    expected: u64,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
    let segments = searcher.segment_readers();
    let inverted_indexes = segments
        .iter()
        .map(|segment| segment.inverted_index(field))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = inverted_indexes
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut occurrences = 0_u64;
    while merged.advance() {
        let uuid = canonical_uuid_term(merged.key(), "event_id")?;
        let projection_digest = query_projection_digest(field, merged.key());
        let mut seen = false;
        for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
            for_each_live_posting(
                &inverted_indexes[segment_ord],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    occurrences = occurrences
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    projection_deltas.accumulate(address, &projection_digest)?;
                    if std::mem::replace(&mut seen, true) {
                        return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
                    }
                    Ok(())
                },
            )?;
        }
    }
    if occurrences != expected {
        return Err(IndexError::InvalidStoredDocumentField("event_id"));
    }
    Ok(())
}

pub(super) fn verify_session_identities(
    searcher: &Searcher,
    fields: [(Field, IdentityFieldRole); 3],
    expected_occurrences: [u64; 3],
    verification_spill: &VerificationSpill,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    COMPLETE_SESSION_ID_TRAVERSALS.with(|count| count.set(count.get().saturating_add(1)));
    let segments = searcher.segment_readers();
    let mut mappings = Vec::with_capacity(fields.len() * segments.len());
    let mut inverted_indexes = Vec::with_capacity(fields.len() * segments.len());
    for (role_index, (field, role)) in fields.into_iter().enumerate() {
        for (segment_ord, segment) in segments.iter().enumerate() {
            inverted_indexes.push(segment.inverted_index(field)?);
            mappings.push((segment_ord, role_index, role, field));
        }
    }
    let streams = inverted_indexes
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut occurrences = [0_u64; 3];
    while merged.advance() {
        let uuid = canonical_uuid_term(merged.key(), "session_id")?;
        let mut digest = None;
        let mut owner = None::<u32>;
        let mut session_core = None;
        let mut session_witness = None;
        let mut live_witnesses = 0_u8;
        let mut saw_session = false;
        for (stream_index, term_info) in merged.current_segment_ords_and_term_infos() {
            let (segment_ord, role_index, role, field) = mappings[stream_index];
            let projection_digest = query_projection_digest(field, merged.key());
            for_each_live_posting(
                &inverted_indexes[stream_index],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    occurrences[role_index] = occurrences[role_index]
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    projection_deltas.accumulate(address, &projection_digest)?;
                    let (identity, source_owner) =
                        identity_for_role(verification_spill, address, role)?;
                    if identity.as_uuid() != uuid {
                        return Err(IndexError::InvalidStoredDocumentField("session_id"));
                    }
                    match digest {
                        None => digest = Some(identity.digest),
                        Some(existing) if existing == identity.digest => {}
                        Some(existing) => {
                            return Err(IndexError::CompactIdentityCollision {
                                kind: "session",
                                uuid,
                                existing_digest: hex(&existing),
                                new_digest: hex(&identity.digest),
                            });
                        }
                    }
                    if let Some(candidate_owner) = source_owner {
                        match owner {
                            Some(existing) if existing != candidate_owner => {
                                return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
                            }
                            None => owner = Some(candidate_owner),
                            _ => {}
                        }
                    }
                    if matches!(role, IdentityFieldRole::Session) {
                        saw_session = true;
                        let facts = session_witness_facts(verification_spill, address)?;
                        session_core = Some(match session_core {
                            Some(existing) => merge_session_witness_facts(existing, facts)?,
                            None => facts,
                        });
                        if facts.witness_present {
                            live_witnesses = live_witnesses
                                .checked_add(1)
                                .ok_or(IndexError::CountOverflow)?;
                            session_witness = Some(match session_witness {
                                Some(existing) => merge_session_witness_facts(existing, facts)?,
                                None => facts,
                            });
                        }
                    }
                    Ok(())
                },
            )?;
        }
        if saw_session
            && (!(1..=4).contains(&live_witnesses)
                || !same_session_witness_facts(session_core, session_witness))
        {
            return Err(IndexError::InvalidStoredDocumentField("session_authority"));
        }
    }
    if occurrences != expected_occurrences {
        return Err(IndexError::InvalidStoredDocumentField("session_id"));
    }
    Ok(())
}

fn same_session_witness_facts(
    left: Option<SessionWitnessFacts>,
    right: Option<SessionWitnessFacts>,
) -> bool {
    matches!(
        (left, right),
        (Some(left), Some(right))
            if left.source_owner == right.source_owner
                && left.parent_session == right.parent_session
                && left.root_session == right.root_session
                && left.kind == right.kind
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionWitnessFacts {
    source_owner: u32,
    parent_session: Option<CompactIdentity>,
    root_session: Option<CompactIdentity>,
    kind: Option<u8>,
    witness_present: bool,
}

fn session_witness_facts(
    verification_spill: &VerificationSpill,
    address: DocAddress,
) -> Result<SessionWitnessFacts> {
    let identities = verification_spill.record(address, "session_authority")?;
    Ok(SessionWitnessFacts {
        source_owner: identities.session_source_ordinal,
        parent_session: identities.parent_session,
        root_session: identities.root_session,
        kind: identities.session_relationship_kind,
        witness_present: identities.session_witness_present,
    })
}

fn merge_session_witness_facts(
    existing: SessionWitnessFacts,
    candidate: SessionWitnessFacts,
) -> Result<SessionWitnessFacts> {
    if existing.source_owner != candidate.source_owner {
        return Err(IndexError::DuplicateSessionIdentity(
            "session witnesses have multiple source owners".to_owned(),
        ));
    }
    Ok(SessionWitnessFacts {
        source_owner: existing.source_owner,
        parent_session: merge_witness_optional(existing.parent_session, candidate.parent_session)?,
        root_session: merge_witness_optional(existing.root_session, candidate.root_session)?,
        kind: merge_witness_optional(existing.kind, candidate.kind)?,
        witness_present: existing.witness_present,
    })
}

fn merge_witness_optional<T: Copy + Eq>(left: Option<T>, right: Option<T>) -> Result<Option<T>> {
    match (left, right) {
        (Some(left), Some(right)) if left == right => Ok(Some(left)),
        (Some(_), Some(_)) => Err(IndexError::ConflictingProviderNativeSessionClaim(
            "one session has contradictory relationship fields",
        )),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn identity_for_role(
    verification_spill: &VerificationSpill,
    address: DocAddress,
    role: IdentityFieldRole,
) -> Result<(CompactIdentity, Option<u32>)> {
    let identities = verification_spill.record(address, "session_id")?;
    match role {
        IdentityFieldRole::Session => {
            Ok((identities.session, Some(identities.session_source_ordinal)))
        }
        IdentityFieldRole::ParentSession => Ok((
            identities
                .parent_session
                .ok_or(IndexError::InvalidStoredDocumentField("parent_session_id"))?,
            None,
        )),
        IdentityFieldRole::RootSession => Ok((
            identities
                .root_session
                .ok_or(IndexError::InvalidStoredDocumentField("root_session_id"))?,
            None,
        )),
    }
}

pub(super) fn for_each_live_posting(
    inverted: &InvertedIndexReader,
    term_info: &tantivy::postings::TermInfo,
    segment_ord: usize,
    segment: &tantivy::SegmentReader,
    mut visit: impl FnMut(DocAddress) -> Result<()>,
) -> Result<()> {
    let mut postings = inverted.read_postings_from_terminfo(term_info, IndexRecordOption::Basic)?;
    let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
    let mut doc_id = postings.doc();
    while doc_id != TERMINATED {
        if !segment.is_deleted(doc_id) {
            visit(DocAddress::new(segment_ord, doc_id))?;
        }
        doc_id = postings.advance();
    }
    Ok(())
}

pub(super) fn canonical_uuid_term(term: &[u8], field: &'static str) -> Result<Uuid> {
    let term =
        std::str::from_utf8(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    let uuid = Uuid::parse_str(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    if uuid.to_string() != term {
        return Err(IndexError::InvalidStoredDocumentField(field));
    }
    Ok(uuid)
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CoreRecord, EventIdentityInput, NativeItemKey,
        NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    };
    use tantivy::{indexer::NoMergePolicy, schema::TantivyDocument};

    use super::*;
    use crate::CompactVerificationIdentities;

    fn source() -> SourceKey {
        SourceKey::derive(
            "codex",
            "session_authority_verifier_test",
            "session",
            1,
            SourceAnchor::provider_native(
                "session-authority-verifier-test",
                TypedKey::utf8("session-authority-verifier-test").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn core_record(source: &SourceKey, session: &str, event: u64) -> CoreRecord {
        let session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8(session).unwrap()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(event)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        CoreRecord::new_selected(
            event_id,
            session_id,
            source.clone(),
            event,
            "message",
            "session-authority-verifier-test-v1",
            "body",
        )
        .unwrap()
    }

    fn session_identity() -> CompactIdentity {
        CompactIdentity { digest: [7; 32] }
    }

    fn session_searcher_and_spill(
        facts: impl IntoIterator<Item = (bool, Option<CompactIdentity>)>,
    ) -> (Searcher, VerificationSpill) {
        let facts = facts.into_iter().collect::<Vec<_>>();
        let schema = crate::lexical_schema();
        let fields = crate::fields_from_schema(&schema).unwrap();
        let index = Index::create_in_ram(schema);
        crate::register_body_analyzer(&index);
        let mut writer = index.writer(20_000_000).unwrap();
        writer.set_merge_policy(Box::<NoMergePolicy>::default());
        let session = session_identity();
        for _ in &facts {
            let mut document = TantivyDocument::default();
            document.add_text(fields.session_id, session.as_uuid().to_string());
            writer.add_document(document).unwrap();
        }
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let spill =
            VerificationSpill::create([u32::try_from(facts.len()).unwrap()].into_iter()).unwrap();
        let mut spill_writer = spill
            .segment_writer(0, u32::try_from(facts.len()).unwrap())
            .unwrap();
        for (doc_id, (witness_present, parent_session)) in facts.into_iter().enumerate() {
            spill_writer
                .write_record(
                    u32::try_from(doc_id).unwrap(),
                    SpillVerificationIdentities {
                        event: CompactIdentity {
                            digest: [u8::try_from(doc_id + 1).unwrap(); 32],
                        },
                        session,
                        parent_session,
                        root_session: None,
                        session_source_ordinal: 0,
                        session_relationship_kind: None,
                        session_witness_present: witness_present,
                    },
                    ProjectionAccumulator::default(),
                )
                .unwrap();
        }
        spill_writer.finish().unwrap();
        (searcher, spill)
    }

    fn assert_session_authority_error(
        searcher: &Searcher,
        spill: &VerificationSpill,
        expected_session_occurrences: u64,
    ) {
        let fields = crate::fields_from_schema(searcher.schema()).unwrap();
        let mut projection_deltas = spill.load_projection_deltas().unwrap();
        assert!(matches!(
            verify_session_identities(
                searcher,
                [
                    (fields.session_id, IdentityFieldRole::Session),
                    (fields.parent_session_id, IdentityFieldRole::ParentSession),
                    (fields.root_session_id, IdentityFieldRole::RootSession),
                ],
                [expected_session_occurrences, 0, 0],
                spill,
                &mut projection_deltas,
            ),
            Err(IndexError::InvalidStoredDocumentField("session_authority"))
        ));
    }

    #[test]
    fn exhaustive_session_verifier_rejects_missing_live_witness() {
        let (searcher, spill) = session_searcher_and_spill([(false, None)]);

        assert_session_authority_error(&searcher, &spill, 1);
    }

    #[test]
    fn exhaustive_session_verifier_rejects_more_than_four_live_witnesses() {
        let (searcher, spill) = session_searcher_and_spill(std::iter::repeat_n((true, None), 5));

        assert_session_authority_error(&searcher, &spill, 5);
    }

    #[test]
    fn exhaustive_session_verifier_rejects_witness_missing_merged_live_core_facts() {
        let parent = CompactIdentity { digest: [9; 32] };
        let (searcher, spill) = session_searcher_and_spill([(false, Some(parent)), (true, None)]);

        assert_session_authority_error(&searcher, &spill, 2);
    }

    #[test]
    fn session_verifier_rejects_witness_key_mismatched_with_core_identity() {
        let source = source();
        let record = core_record(&source, "session", 1);
        let witness_record = core_record(&source, "other-session", 2);
        let authority = SessionAuthorityKey::for_core_record(&witness_record)
            .unwrap()
            .into_bytes();
        let stored = VerificationRecord {
            core_record: record,
            source_owner: crate::source_token(&source),
            core_record_leaf: [0; 32],
            source_event_order: [0; crate::SOURCE_EVENT_ORDER_KEY_LEN],
            session_event_order: [0; crate::SESSION_EVENT_ORDER_KEY_LEN],
            session_authority: Some(authority),
            semantic_event_order: [0; crate::SEMANTIC_EVENT_ORDER_KEY_LEN],
            event_range_order: [0; crate::EVENT_RANGE_ORDER_KEY_LEN],
            identities: CompactVerificationIdentities {
                event: CompactIdentity { digest: [0; 32] },
                session: CompactIdentity { digest: [0; 32] },
                parent_session: None,
                root_session: None,
                session_source_owner: source.identity().digest(),
            },
            stored_core_bytes: 0,
        };

        assert!(matches!(
            verify_session_witness_key(&stored),
            Err(IndexError::InvalidStoredDocumentField("session_authority"))
        ));
    }
}

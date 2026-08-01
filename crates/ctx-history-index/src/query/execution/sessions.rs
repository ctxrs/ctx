use super::*;

impl VerifiedIndex {
    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<EventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        Ok(events)
    }

    /// Returns every event in one session with complete stored Core data.
    pub fn core_events_for_session(&self, session_id: Uuid) -> Result<Vec<CoreEventRecord>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let mut records = self.core_event_records_for_query(&query, fields)?;
        sort_core_events_for_session(&mut records);
        Ok(records)
    }

    /// Returns every deterministic coordinate for one session without stored
    /// Core bodies. Presentation callers must use the bounded prefix/window
    /// selectors instead; this complete enumeration is for bounded maintenance
    /// contexts that already constrain session cardinality.
    pub fn session_event_coordinates(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<SessionEventCoordinate>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let segments = self.searcher.segment_readers();
        let mut coordinates = Vec::with_capacity(addresses.len());
        for address in addresses {
            let segment = segments
                .get(address.segment_ord as usize)
                .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?;
            let event_id_high = segment
                .fast_fields()
                .u64(EVENT_ID_HIGH_FIELD)?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField("event_id"))?;
            let event_id_low = segment
                .fast_fields()
                .u64(EVENT_ID_LOW_FIELD)?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField("event_id"))?;
            let event_sequence = segment
                .fast_fields()
                .u64("event_sequence")?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField("event_sequence"))?;
            let occurred_at_unix_ms = segment
                .fast_fields()
                .i64("occurred_at_unix_ms")?
                .first(address.doc_id);
            let compact_event_id =
                Uuid::from_u128((u128::from(event_id_high) << 64) | u128::from(event_id_low));
            coordinates.push(SessionEventCoordinate {
                event_id: compact_event_id,
                event_sequence,
                occurred_at_unix_ms,
            });
        }
        coordinates.sort_by(|left, right| {
            left.event_sequence
                .cmp(&right.event_sequence)
                .then_with(|| left.occurred_at_unix_ms.cmp(&right.occurred_at_unix_ms))
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        if let Some(pair) = coordinates
            .windows(2)
            .find(|pair| pair[0].event_id == pair[1].event_id)
        {
            return Err(IndexError::DuplicateEventIdentity(
                pair[1].event_id.to_string(),
            ));
        }
        Ok(coordinates)
    }

    /// Returns the first `limit` deterministic coordinates for one session
    /// without scoring or retaining the complete session. One stored record is
    /// decoded to authenticate the full stable identity behind the compact
    /// session UUID; coordinate traversal itself stays body-free.
    pub fn session_event_coordinate_prefix(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> Result<Vec<SessionEventCoordinate>> {
        if !(1..=MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS).contains(&limit) {
            return Err(IndexError::InvalidSessionEventCoordinateLimit {
                requested: limit,
                maximum: MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS,
            });
        }
        validate_session_event_coordinate_fast_fields(&self.searcher)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let Some(stable_session_id) = self.stable_session_id(session_id, fields)? else {
            return Ok(Vec::new());
        };
        let candidates =
            self.session_event_addresses_after(stable_session_id, None, limit, fields)?;
        let coordinates = candidates
            .into_iter()
            .map(|candidate| session_event_coordinate(candidate.order))
            .collect::<Vec<_>>();
        validate_session_event_coordinates(&coordinates)?;
        Ok(coordinates)
    }

    /// Returns a deterministic body-free window centered on one exact event.
    /// At most 101 coordinates are retained and only one stable-identity
    /// bootstrap record is decoded regardless of session cardinality.
    pub fn session_event_coordinate_window(
        &self,
        session_id: Uuid,
        selected_event_id: Uuid,
        before: usize,
        after: usize,
    ) -> Result<Option<Vec<SessionEventCoordinate>>> {
        let requested = before
            .checked_add(after)
            .and_then(|neighbors| neighbors.checked_add(1))
            .unwrap_or(usize::MAX);
        if !(1..=MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS).contains(&requested) {
            return Err(IndexError::InvalidSessionEventCoordinateLimit {
                requested,
                maximum: MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
            });
        }
        validate_session_event_coordinate_fast_fields(&self.searcher)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let selected_query = BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.session_id, &session_id.to_string()),
                    IndexRecordOption::Basic,
                )),
            ),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.event_id, &selected_event_id.to_string()),
                    IndexRecordOption::Basic,
                )),
            ),
        ]);
        let selected_collector = TopDocs::with_limit(2).tweak_score(session_event_coordinate_score);
        type SelectedCoordinateHit = (SessionEventCoordinateSortKey, DocAddress);
        let selected_hits: Vec<SelectedCoordinateHit> =
            self.searcher.search(&selected_query, &selected_collector)?;
        let selected_sort_key = match selected_hits.as_slice() {
            [] => return Ok(None),
            [(sort_key, _)] => *sort_key,
            _ => {
                return Err(IndexError::DuplicateEventIdentity(
                    selected_event_id.to_string(),
                ));
            }
        };
        if SessionEventCoordinate::from_sort_key(selected_sort_key).event_id != selected_event_id {
            return Err(IndexError::InvalidStoredDocumentField("event_id"));
        }
        let stable_session_id = self
            .stable_session_id(session_id, fields)?
            .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?;
        let selected_order = session_event_order_key(stable_session_id, selected_sort_key)?;

        let preceding = if before == 0 {
            Vec::new()
        } else {
            self.session_event_addresses_before(stable_session_id, selected_order, before, fields)?
                .into_iter()
                .map(|candidate| session_event_coordinate(candidate.order))
                .collect::<Vec<_>>()
        };
        let following = if after == 0 {
            Vec::new()
        } else {
            self.session_event_addresses_after(
                stable_session_id,
                Some(selected_order),
                after,
                fields,
            )?
            .into_iter()
            .map(|candidate| session_event_coordinate(candidate.order))
            .collect::<Vec<_>>()
        };
        let selected = session_event_coordinate(selected_order);
        let mut preceding = preceding;
        preceding.push(selected);
        preceding.extend(following);
        validate_session_event_coordinates(&preceding)?;
        Ok(Some(preceding))
    }

    /// Returns one session only when its event cardinality is within a caller
    /// budget.
    ///
    /// The count pass reads postings without constructing stored event
    /// records. This lets best-effort consumers decline pathological sessions
    /// before allocating metadata for every event.
    pub fn events_for_session_if_bounded(
        &self,
        session_id: Uuid,
        maximum_events: usize,
    ) -> Result<Option<Vec<EventRecord>>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        if self.searcher.search(&query, &Count)? > maximum_events {
            return Ok(None);
        }
        let mut events = self.event_records_for_query(&query, fields)?;
        sort_events_for_session(&mut events);
        Ok(Some(events))
    }

    /// Returns exact normalized Core-content bytes for a nonempty session only
    /// when its cardinality is within the caller's bound. This reads indexed
    /// size metadata and never loads or decodes stored Core records.
    pub fn core_content_bytes_for_session_if_bounded(
        &self,
        session_id: Uuid,
        maximum_events: usize,
    ) -> Result<Option<usize>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let count = self.searcher.search(&query, &Count)?;
        if count == 0 || count > maximum_events {
            return Ok(None);
        }
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let segments = self.searcher.segment_readers();
        let mut total = 0_usize;
        for address in addresses {
            let segment = segments
                .get(address.segment_ord as usize)
                .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?;
            let value = segment
                .fast_fields()
                .u64(CORE_CONTENT_BYTES_FIELD)?
                .first(address.doc_id)
                .ok_or(IndexError::InvalidStoredDocumentField(
                    CORE_CONTENT_BYTES_FIELD,
                ))?;
            let value = usize::try_from(value).map_err(|_| IndexError::CountOverflow)?;
            total = total.checked_add(value).ok_or(IndexError::CountOverflow)?;
        }
        Ok(Some(total))
    }

    /// Returns complete Core events only when session cardinality is within a
    /// caller budget, without materializing documents for a declined session.
    pub fn core_events_for_session_if_bounded(
        &self,
        session_id: Uuid,
        maximum_events: usize,
    ) -> Result<Option<Vec<CoreEventRecord>>> {
        Ok(self
            .core_events_for_session_within_budget(session_id, maximum_events, usize::MAX)?
            .map(|(records, _)| records))
    }

    /// Returns one complete session and its exact stored-Core byte count only
    /// when both caller budgets admit it. A declined session never exposes a
    /// partial event list, and retained decoded records remain within the byte
    /// budget plus at most the one record currently being considered.
    pub fn core_events_for_session_within_budget(
        &self,
        session_id: Uuid,
        maximum_events: usize,
        maximum_stored_core_bytes: usize,
    ) -> Result<Option<(Vec<CoreEventRecord>, usize)>> {
        let fields = fields_from_schema(self.searcher.schema())?;
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let count = self.searcher.search(&query, &Count)?;
        if count > maximum_events {
            return Ok(None);
        }
        let addresses = self.searcher.search(&query, &DocSetCollector)?;
        let mut records = Vec::with_capacity(addresses.len());
        let mut stored_core_bytes = 0_usize;
        for address in addresses {
            let (record, record_stored_core_bytes) =
                stored_core_event_record_with_size(&self.searcher, address, fields)?;
            if record.session_id.as_uuid() != session_id {
                return Err(IndexError::InvalidStoredDocumentField("session_id"));
            }
            let Some(next_stored_core_bytes) =
                stored_core_bytes.checked_add(record_stored_core_bytes)
            else {
                return Ok(None);
            };
            if next_stored_core_bytes > maximum_stored_core_bytes {
                return Ok(None);
            }
            stored_core_bytes = next_stored_core_bytes;
            records.push(record);
        }
        sort_core_events_for_session(&mut records);
        Ok(Some((records, stored_core_bytes)))
    }

    /// Resolves the full stable identity from one live compact-session
    /// posting. Verified generations guarantee every posting for a compact
    /// session UUID has the same full identity, so no session-wide collector
    /// is needed to establish the indexed order-key prefix.
    fn stable_session_id(
        &self,
        session_id: Uuid,
        fields: Fields,
    ) -> Result<Option<StableEntityId>> {
        let term = Term::from_field_text(fields.session_id, &session_id.to_string());
        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(fields.session_id)?;
            let Some(mut postings) = inverted.read_postings(&term, IndexRecordOption::Basic)?
            else {
                continue;
            };
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                if !segment.is_deleted(doc_id) {
                    let segment_ord =
                        u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                    let event = stored_event_record(
                        &self.searcher,
                        DocAddress::new(segment_ord, doc_id),
                        fields,
                    )?;
                    if event.session_id.as_uuid() != session_id {
                        return Err(IndexError::InvalidStoredDocumentField("session_id"));
                    }
                    return Ok(Some(event.session_id));
                }
                doc_id = postings.advance();
            }
        }
        Ok(None)
    }

    /// Pulls at most `capacity` live coordinates from one exact session range
    /// in global ascending order. Each segment stream seeks directly to the
    /// requested lower bound and `TermMerger` retains only one frontier per
    /// segment.
    fn session_event_addresses_after(
        &self,
        session_id: StableEntityId,
        after: Option<SessionEventOrderKey>,
        capacity: usize,
        fields: Fields,
    ) -> Result<Vec<SessionEventAddressCandidate>> {
        let segments = self.searcher.segment_readers();
        let session_prefix = SessionEventOrderKey::session_prefix(session_id)?;
        let range_end = SessionEventOrderKey::session_range_end(session_id)?;
        let inverted_indexes = segments
            .iter()
            .map(|segment| segment.inverted_index(fields.session_event_order))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let streams = inverted_indexes
            .iter()
            .map(|inverted| match after.as_ref() {
                Some(after) => inverted
                    .terms()
                    .range()
                    .gt(after.as_bytes())
                    .lt(&range_end)
                    .into_stream(),
                None => inverted
                    .terms()
                    .range()
                    .ge(session_prefix)
                    .lt(&range_end)
                    .into_stream(),
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut merged = TermMerger::new(streams);
        session_event_address_page(
            session_id,
            capacity,
            &mut merged,
            &inverted_indexes,
            segments,
        )
    }

    /// Pulls at most `capacity` live coordinates below an exact order key.
    /// Tantivy's merger is ascending-only, so this keeps one reverse frontier
    /// per segment and merges those frontiers by their largest current key.
    fn session_event_addresses_before(
        &self,
        session_id: StableEntityId,
        before: SessionEventOrderKey,
        capacity: usize,
        fields: Fields,
    ) -> Result<Vec<SessionEventAddressCandidate>> {
        let segments = self.searcher.segment_readers();
        let session_prefix = SessionEventOrderKey::session_prefix(session_id)?;
        let inverted_indexes = segments
            .iter()
            .map(|segment| segment.inverted_index(fields.session_event_order))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut streams = inverted_indexes
            .iter()
            .map(|inverted| {
                inverted
                    .terms()
                    .range()
                    .ge(session_prefix)
                    .lt(before.as_bytes())
                    .backward()
                    .into_stream()
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut active = streams
            .iter_mut()
            .map(|stream| stream.advance())
            .collect::<Vec<_>>();
        let mut candidates = Vec::with_capacity(capacity);

        while candidates.len() < capacity {
            let Some(current_key) = (0..streams.len())
                .filter(|segment_ord| active[*segment_ord])
                .map(|segment_ord| streams[segment_ord].key())
                .max()
                .map(<[u8]>::to_vec)
            else {
                break;
            };
            let order = SessionEventOrderKey::decode_for_session(session_id, &current_key)?;
            #[cfg(test)]
            {
                SESSION_EVENT_ORDER_TERM_VISITS
                    .set(SESSION_EVENT_ORDER_TERM_VISITS.get().saturating_add(1));
                SESSION_EVENT_ORDER_VISITED_SEQUENCES
                    .with(|sequences| sequences.borrow_mut().push(order.event_sequence()));
            }

            let mut address = None;
            let mut matching_segments = Vec::new();
            for segment_ord in 0..streams.len() {
                if !active[segment_ord] || streams[segment_ord].key() != current_key {
                    continue;
                }
                matching_segments.push(segment_ord);
                let inverted = inverted_indexes.get(segment_ord).ok_or(
                    IndexError::InvalidStoredDocumentField(SESSION_EVENT_ORDER_FIELD),
                )?;
                let segment =
                    segments
                        .get(segment_ord)
                        .ok_or(IndexError::InvalidStoredDocumentField(
                            SESSION_EVENT_ORDER_FIELD,
                        ))?;
                let mut postings = inverted.read_postings_from_terminfo(
                    streams[segment_ord].value(),
                    IndexRecordOption::Basic,
                )?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        let segment_ord =
                            u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
                        if address
                            .replace(DocAddress::new(segment_ord, doc_id))
                            .is_some()
                        {
                            return Err(IndexError::InvalidStoredDocumentField(
                                SESSION_EVENT_ORDER_FIELD,
                            ));
                        }
                    }
                    doc_id = postings.advance();
                }
            }
            for segment_ord in matching_segments {
                active[segment_ord] = streams[segment_ord].advance();
            }
            if let Some(address) = address {
                candidates.push(SessionEventAddressCandidate { order, address });
            }
        }
        candidates.reverse();
        Ok(candidates)
    }
}

fn session_event_coordinate(order: SessionEventOrderKey) -> SessionEventCoordinate {
    let event_id = order.event_id().as_u128();
    SessionEventCoordinate::from_sort_key((
        order.event_sequence(),
        order.occurred_at_unix_ms(),
        (event_id >> 64) as u64,
        event_id as u64,
    ))
}

fn session_event_order_key(
    session_id: StableEntityId,
    sort_key: SessionEventCoordinateSortKey,
) -> Result<SessionEventOrderKey> {
    let coordinate = SessionEventCoordinate::from_sort_key(sort_key);
    let mut encoded = Vec::with_capacity(StableEntityId::CANONICAL_LEN + 33);
    encoded.extend_from_slice(&SessionEventOrderKey::session_prefix(session_id)?);
    encoded.extend_from_slice(&coordinate.event_sequence.to_be_bytes());
    match coordinate.occurred_at_unix_ms {
        None => encoded.extend_from_slice(&[0_u8; 9]),
        Some(occurred_at_unix_ms) => {
            encoded.push(1);
            let sortable = (occurred_at_unix_ms as u64) ^ (1_u64 << 63);
            encoded.extend_from_slice(&sortable.to_be_bytes());
        }
    }
    encoded.extend_from_slice(coordinate.event_id.as_bytes());
    SessionEventOrderKey::decode_for_session(session_id, &encoded)
}

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
    /// without decoding stored Core records or retaining the complete session.
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
        let query = TermQuery::new(
            Term::from_field_text(fields.session_id, &session_id.to_string()),
            IndexRecordOption::Basic,
        );
        let collector = TopDocs::with_limit(limit).tweak_score(move |segment_reader| {
            let score = session_event_coordinate_score(segment_reader);
            move |doc, original_score| Reverse(score(doc, original_score))
        });
        type CoordinateHit = (Reverse<SessionEventCoordinateSortKey>, DocAddress);
        let hits: Vec<CoordinateHit> = self.searcher.search(&query, &collector)?;
        let coordinates = hits
            .into_iter()
            .map(|(Reverse(sort_key), _)| SessionEventCoordinate::from_sort_key(sort_key))
            .collect::<Vec<_>>();
        validate_session_event_coordinates(&coordinates)?;
        Ok(coordinates)
    }

    /// Returns a deterministic body-free window centered on one exact event.
    /// At most 101 coordinates are retained regardless of session cardinality.
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
        let session_term = || {
            TermQuery::new(
                Term::from_field_text(fields.session_id, &session_id.to_string()),
                IndexRecordOption::Basic,
            )
        };
        let selected_query = BooleanQuery::new(vec![
            (Occur::Must, Box::new(session_term())),
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
        let selected = SessionEventCoordinate::from_sort_key(selected_sort_key);
        if selected.event_id != selected_event_id {
            return Err(IndexError::InvalidStoredDocumentField("event_id"));
        }

        let mut preceding = if before == 0 {
            Vec::new()
        } else {
            let collector = TopDocs::with_limit(before).tweak_score(move |segment_reader| {
                let score = session_event_coordinate_score(segment_reader);
                move |doc, original_score| {
                    let sort_key = score(doc, original_score);
                    (sort_key < selected_sort_key, sort_key)
                }
            });
            type PrecedingHit = ((bool, SessionEventCoordinateSortKey), DocAddress);
            let hits: Vec<PrecedingHit> = self.searcher.search(&session_term(), &collector)?;
            let mut coordinates = hits
                .into_iter()
                .filter(|((is_preceding, _), _)| *is_preceding)
                .map(|((_, sort_key), _)| SessionEventCoordinate::from_sort_key(sort_key))
                .collect::<Vec<_>>();
            coordinates.reverse();
            coordinates
        };
        let following = if after == 0 {
            Vec::new()
        } else {
            let collector = TopDocs::with_limit(after).tweak_score(move |segment_reader| {
                let score = session_event_coordinate_score(segment_reader);
                move |doc, original_score| {
                    let sort_key = score(doc, original_score);
                    (sort_key > selected_sort_key, Reverse(sort_key))
                }
            });
            type FollowingHit = ((bool, Reverse<SessionEventCoordinateSortKey>), DocAddress);
            let hits: Vec<FollowingHit> = self.searcher.search(&session_term(), &collector)?;
            hits.into_iter()
                .filter(|((is_following, _), _)| *is_following)
                .map(|((_, Reverse(sort_key)), _)| SessionEventCoordinate::from_sort_key(sort_key))
                .collect::<Vec<_>>()
        };
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
}

use std::{collections::HashSet, sync::Arc};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EventDocAnchor {
    pub(super) occurred_at_unix_ms: Option<i64>,
    pub(super) event_sequence: u64,
    pub(super) event_id: Uuid,
    pub(super) address: DocAddress,
}

impl EventDocAnchor {
    pub(super) fn is_earlier_than(self, other: Self) -> bool {
        (self.event_sequence, self.occurred_at_unix_ms, self.event_id)
            < (
                other.event_sequence,
                other.occurred_at_unix_ms,
                other.event_id,
            )
    }

    pub(super) fn is_later_than(self, other: Self) -> bool {
        (self.occurred_at_unix_ms, self.event_sequence, self.event_id)
            > (
                other.occurred_at_unix_ms,
                other.event_sequence,
                other.event_id,
            )
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SessionFastAggregate {
    pub(super) events: u64,
    pub(super) started_at_unix_ms: Option<i64>,
    pub(super) last_activity_at_unix_ms: Option<i64>,
    pub(super) max_event_sequence: u64,
    pub(super) first_event: EventDocAnchor,
    pub(super) last_event: EventDocAnchor,
}

impl SessionFastAggregate {
    fn singleton(anchor: EventDocAnchor) -> Self {
        Self {
            events: 1,
            started_at_unix_ms: anchor.occurred_at_unix_ms,
            last_activity_at_unix_ms: anchor.occurred_at_unix_ms,
            max_event_sequence: anchor.event_sequence,
            first_event: anchor,
            last_event: anchor,
        }
    }

    fn record_event(&mut self, anchor: EventDocAnchor) -> std::result::Result<(), ()> {
        self.events = self.events.checked_add(1).ok_or(())?;
        self.started_at_unix_ms =
            min_timestamp(self.started_at_unix_ms, anchor.occurred_at_unix_ms);
        self.last_activity_at_unix_ms =
            max_timestamp(self.last_activity_at_unix_ms, anchor.occurred_at_unix_ms);
        self.max_event_sequence = self.max_event_sequence.max(anchor.event_sequence);
        if anchor.is_earlier_than(self.first_event) {
            self.first_event = anchor;
        }
        if anchor.is_later_than(self.last_event) {
            self.last_event = anchor;
        }
        Ok(())
    }

    fn merge(&mut self, other: Self) -> std::result::Result<(), ()> {
        self.events = self.events.checked_add(other.events).ok_or(())?;
        self.started_at_unix_ms = min_timestamp(self.started_at_unix_ms, other.started_at_unix_ms);
        self.last_activity_at_unix_ms = max_timestamp(
            self.last_activity_at_unix_ms,
            other.last_activity_at_unix_ms,
        );
        self.max_event_sequence = self.max_event_sequence.max(other.max_event_sequence);
        if other.first_event.is_earlier_than(self.first_event) {
            self.first_event = other.first_event;
        }
        if other.last_event.is_later_than(self.last_event) {
            self.last_event = other.last_event;
        }
        Ok(())
    }
}

fn min_timestamp(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn max_timestamp(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

pub(super) fn validate_session_list_fast_fields(searcher: &tantivy::Searcher) -> Result<()> {
    for segment in searcher.segment_readers() {
        let fast = segment.fast_fields();
        fast.u64(SESSION_ID_HIGH_FIELD)?;
        fast.u64(SESSION_ID_LOW_FIELD)?;
        fast.u64(EVENT_ID_HIGH_FIELD)?;
        fast.u64(EVENT_ID_LOW_FIELD)?;
        fast.u64(EVENT_SEQUENCE_FIELD)?;
        fast.i64(OCCURRED_AT_UNIX_MS_FIELD)?;
    }
    Ok(())
}

pub(super) struct SessionIdSetCollector;

pub(super) struct SessionIdSetSegmentCollector {
    high: tantivy::columnar::Column<u64>,
    low: tantivy::columnar::Column<u64>,
    session_ids: HashSet<Uuid>,
    invalid: bool,
}

pub(super) struct SessionIdSetSegmentFruit {
    session_ids: HashSet<Uuid>,
    invalid: bool,
}

impl Collector for SessionIdSetCollector {
    type Fruit = HashSet<Uuid>;
    type Child = SessionIdSetSegmentCollector;

    fn for_segment(
        &self,
        _segment_local_id: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(SessionIdSetSegmentCollector {
            high: segment.fast_fields().u64(SESSION_ID_HIGH_FIELD)?,
            low: segment.fast_fields().u64(SESSION_ID_LOW_FIELD)?,
            session_ids: HashSet::new(),
            invalid: false,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<SessionIdSetSegmentFruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut merged = HashSet::new();
        for fruit in segment_fruits {
            if fruit.invalid {
                return Err(tantivy::TantivyError::InvalidArgument(
                    "session identity fast field is absent".to_owned(),
                ));
            }
            merged.extend(fruit.session_ids);
        }
        Ok(merged)
    }
}

impl SegmentCollector for SessionIdSetSegmentCollector {
    type Fruit = SessionIdSetSegmentFruit;

    fn collect(&mut self, doc: DocId, _score: Score) {
        let (Some(high), Some(low)) = (self.high.first(doc), self.low.first(doc)) else {
            self.invalid = true;
            return;
        };
        self.session_ids
            .insert(Uuid::from_u128((u128::from(high) << 64) | u128::from(low)));
    }

    fn harvest(self) -> Self::Fruit {
        SessionIdSetSegmentFruit {
            session_ids: self.session_ids,
            invalid: self.invalid,
        }
    }
}

pub(super) struct SessionActivityCollector {
    pub(super) allowed_sessions: Option<Arc<HashSet<Uuid>>>,
    pub(super) excluded_sessions: Option<Arc<HashSet<Uuid>>>,
}

pub(super) struct SessionActivitySegmentCollector {
    segment_ord: SegmentOrdinal,
    session_high: tantivy::columnar::Column<u64>,
    session_low: tantivy::columnar::Column<u64>,
    event_high: tantivy::columnar::Column<u64>,
    event_low: tantivy::columnar::Column<u64>,
    event_sequence: tantivy::columnar::Column<u64>,
    occurred_at_unix_ms: tantivy::columnar::Column<i64>,
    allowed_sessions: Option<Arc<HashSet<Uuid>>>,
    excluded_sessions: Option<Arc<HashSet<Uuid>>>,
    aggregates: HashMap<Uuid, SessionFastAggregate>,
    invalid: bool,
    overflowed: bool,
}

pub(super) struct SessionActivitySegmentFruit {
    aggregates: HashMap<Uuid, SessionFastAggregate>,
    invalid: bool,
    overflowed: bool,
}

impl Collector for SessionActivityCollector {
    type Fruit = HashMap<Uuid, SessionFastAggregate>;
    type Child = SessionActivitySegmentCollector;

    fn for_segment(
        &self,
        segment_local_id: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let fast = segment.fast_fields();
        Ok(SessionActivitySegmentCollector {
            segment_ord: segment_local_id,
            session_high: fast.u64(SESSION_ID_HIGH_FIELD)?,
            session_low: fast.u64(SESSION_ID_LOW_FIELD)?,
            event_high: fast.u64(EVENT_ID_HIGH_FIELD)?,
            event_low: fast.u64(EVENT_ID_LOW_FIELD)?,
            event_sequence: fast.u64(EVENT_SEQUENCE_FIELD)?,
            occurred_at_unix_ms: fast.i64(OCCURRED_AT_UNIX_MS_FIELD)?,
            allowed_sessions: self.allowed_sessions.clone(),
            excluded_sessions: self.excluded_sessions.clone(),
            aggregates: HashMap::new(),
            invalid: false,
            overflowed: false,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<SessionActivitySegmentFruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut merged: HashMap<Uuid, SessionFastAggregate> = HashMap::new();
        for fruit in segment_fruits {
            if fruit.invalid {
                return Err(tantivy::TantivyError::InvalidArgument(
                    "session list fast field is absent".to_owned(),
                ));
            }
            if fruit.overflowed {
                return Err(tantivy::TantivyError::InvalidArgument(
                    "session event count overflowed".to_owned(),
                ));
            }
            for (session_id, aggregate) in fruit.aggregates {
                match merged.entry(session_id) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(aggregate);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if entry.get_mut().merge(aggregate).is_err() {
                            return Err(tantivy::TantivyError::InvalidArgument(
                                "session event count overflowed".to_owned(),
                            ));
                        }
                    }
                }
            }
        }
        Ok(merged)
    }
}

impl SegmentCollector for SessionActivitySegmentCollector {
    type Fruit = SessionActivitySegmentFruit;

    fn collect(&mut self, doc: DocId, _score: Score) {
        let (
            Some(session_high),
            Some(session_low),
            Some(event_high),
            Some(event_low),
            Some(event_sequence),
        ) = (
            self.session_high.first(doc),
            self.session_low.first(doc),
            self.event_high.first(doc),
            self.event_low.first(doc),
            self.event_sequence.first(doc),
        )
        else {
            self.invalid = true;
            return;
        };
        let session_id =
            Uuid::from_u128((u128::from(session_high) << 64) | u128::from(session_low));
        if self
            .excluded_sessions
            .as_deref()
            .is_some_and(|excluded| excluded.contains(&session_id))
        {
            return;
        }
        if self
            .allowed_sessions
            .as_deref()
            .is_some_and(|allowed| !allowed.contains(&session_id))
        {
            return;
        }
        let anchor = EventDocAnchor {
            occurred_at_unix_ms: self.occurred_at_unix_ms.first(doc),
            event_sequence,
            event_id: Uuid::from_u128((u128::from(event_high) << 64) | u128::from(event_low)),
            address: DocAddress::new(self.segment_ord, doc),
        };
        match self.aggregates.entry(session_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(SessionFastAggregate::singleton(anchor));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if entry.get_mut().record_event(anchor).is_err() {
                    self.overflowed = true;
                }
            }
        }
    }

    fn harvest(self) -> Self::Fruit {
        SessionActivitySegmentFruit {
            aggregates: self.aggregates,
            invalid: self.invalid,
            overflowed: self.overflowed,
        }
    }
}

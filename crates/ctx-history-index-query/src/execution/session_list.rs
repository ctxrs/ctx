use std::{collections::HashSet, sync::Arc};

use ctx_history_index_format::SessionAuthorityKey;

use super::*;

const WORKSPACE_TERM_SET_THRESHOLD: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventDocAnchor {
    occurred_at_unix_ms: Option<i64>,
    event_sequence: u64,
    event_id: Uuid,
    address: DocAddress,
}

impl EventDocAnchor {
    fn is_earlier_than(self, other: Self) -> bool {
        (self.event_sequence, self.occurred_at_unix_ms, self.event_id)
            < (
                other.event_sequence,
                other.occurred_at_unix_ms,
                other.event_id,
            )
    }

    fn is_later_than(self, other: Self) -> bool {
        (self.occurred_at_unix_ms, self.event_sequence, self.event_id)
            > (
                other.occurred_at_unix_ms,
                other.event_sequence,
                other.event_id,
            )
    }
}

#[derive(Debug, Clone, Copy)]
struct SessionFastAggregate {
    events: u64,
    started_at_unix_ms: Option<i64>,
    last_activity_at_unix_ms: Option<i64>,
    max_event_sequence: u64,
    first_event: EventDocAnchor,
    last_event: EventDocAnchor,
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

impl VerifiedIndex {
    /// Lists indexed sessions from this pinned verified generation, ordered
    /// newest activity first.
    ///
    /// Session selection and ranking read only Tantivy fast fields and
    /// inverted indexes; stored Core records are decoded only for the at-most
    /// `limit` winning sessions.
    pub fn list_sessions(
        &self,
        filters: &SessionListFilters,
        limit: usize,
    ) -> std::result::Result<SessionListPage, SessionListError> {
        if !(1..=MAX_SESSION_LIST_LIMIT).contains(&limit) {
            return Err(SessionListError::InvalidLimit {
                requested: limit,
                maximum: MAX_SESSION_LIST_LIMIT,
            });
        }
        let provider = filters
            .provider
            .as_deref()
            .map(|value| validated_filter_text("provider", value))
            .transpose()?;
        let workspace = filters
            .workspace
            .as_deref()
            .map(|value| validated_filter_text("workspace", value))
            .transpose()?;

        let fields = fields_from_schema(self.searcher.schema())?;
        validate_session_list_fast_fields(&self.searcher)?;

        let excluded_subagents = if filters.no_subagents {
            Some(Arc::new(
                self.collect_subagent_session_ids(fields, provider)?,
            ))
        } else {
            None
        };

        let workspace_matches = if let Some(workspace) = workspace {
            let matches = self.collect_workspace_matching_sessions(
                fields,
                provider,
                workspace,
                excluded_subagents.clone(),
            )?;
            if matches.is_empty() {
                return Ok(SessionListPage {
                    generation_id: self.generation_id.clone(),
                    truncated: false,
                    sessions: Vec::new(),
                });
            }
            Some(Arc::new(matches))
        } else {
            None
        };

        let activity_query = build_activity_query(
            fields,
            provider,
            filters.since_unix_ms,
            workspace_matches.as_deref(),
        );
        let aggregates = self
            .searcher
            .search(
                activity_query.as_ref(),
                &SessionActivityCollector {
                    allowed_sessions: workspace_matches.clone(),
                    excluded_sessions: excluded_subagents,
                },
            )
            .map_err(IndexError::from)?;

        if aggregates.is_empty() {
            return Ok(SessionListPage {
                generation_id: self.generation_id.clone(),
                truncated: false,
                sessions: Vec::new(),
            });
        }

        let truncated = aggregates.len() > limit;
        let mut ranked = aggregates.into_iter().collect::<Vec<_>>();
        if ranked.len() > limit {
            ranked.select_nth_unstable_by(limit, compare_ranked_sessions);
            ranked.truncate(limit);
        }
        ranked.sort_unstable_by(compare_ranked_sessions);

        // When `since_unix_ms` narrowed the first pass, compute exact
        // whole-session counts, start timestamps, and first/last doc anchors
        // for ONLY the winning sessions using fast fields (no Core decode).
        let whole_session_aggregates = if filters.since_unix_ms.is_some() {
            let winning_ids = ranked
                .iter()
                .map(|(session_id, _)| *session_id)
                .collect::<Vec<_>>();
            Some(self.collect_whole_session_aggregates(fields, &winning_ids)?)
        } else {
            None
        };

        let mut hydrated = Vec::with_capacity(ranked.len());
        let mut coordinates = Vec::with_capacity(ranked.len());
        for (session_id, filtered_aggregate) in ranked {
            let aggregate = match whole_session_aggregates.as_ref() {
                Some(map) => map
                    .get(&session_id)
                    .copied()
                    .ok_or(IndexError::InvalidStoredDocumentField("session_id"))?,
                None => filtered_aggregate,
            };
            let workspace_doc = workspace_matches
                .as_ref()
                .and_then(|matches| matches.get(&session_id))
                .map(|anchor| anchor.address);
            let partial =
                self.hydrate_winning_session(session_id, aggregate, workspace_doc, fields)?;
            coordinates.push((partial.session_id, partial.source_owner));
            hydrated.push(partial);
        }

        let grouping_claims = self.session_grouping_claims(&coordinates)?;
        let sessions = hydrated
            .into_iter()
            .zip(grouping_claims)
            .map(|(partial, claims)| SessionSummaryRecord {
                session_id: partial.session_id,
                provider: partial.provider,
                provider_key: partial.provider_key,
                source_id: partial.source_id,
                provider_session_id: partial.provider_session_id,
                parent_session_id: claims.parent_session_id.or(partial.parent_session_id),
                root_session_id: claims.root_session_id.or(partial.root_session_id),
                cwd: partial.cwd,
                project: partial.project,
                branch: partial.branch,
                started_at_unix_ms: partial.started_at_unix_ms,
                last_activity_at_unix_ms: partial.last_activity_at_unix_ms,
                events: partial.events,
            })
            .collect();

        Ok(SessionListPage {
            generation_id: self.generation_id.clone(),
            truncated,
            sessions,
        })
    }

    fn collect_subagent_session_ids(
        &self,
        fields: Fields,
        provider: Option<&str>,
    ) -> Result<HashSet<Uuid>> {
        let mut subagents = HashSet::new();
        for segment in self.searcher.segment_readers() {
            let inverted = segment.inverted_index(fields.session_authority)?;
            let mut terms = inverted.terms().stream()?;
            while terms.advance() {
                let key = SessionAuthorityKey::decode(terms.key())?;
                if key.direct_claims()?.parent_session_id.is_none() {
                    continue;
                }
                let mut postings = inverted
                    .read_postings_from_terminfo(terms.value(), IndexRecordOption::Basic)?;
                let mut doc_id = postings.doc();
                while doc_id != TERMINATED {
                    if !segment.is_deleted(doc_id) {
                        let (session_id, _) = key.identities()?;
                        subagents.insert(session_id.as_uuid());
                        break;
                    }
                    doc_id = postings.advance();
                }
            }
        }

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.agent_scope, CoreAgentScope::Subagent.as_str()),
                IndexRecordOption::Basic,
            )),
        )];
        if let Some(provider) = provider {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.provider, provider),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        let query = BooleanQuery::new(clauses);
        let scope_subagents = self.searcher.search(&query, &SessionIdSetCollector)?;
        subagents.extend(scope_subagents);
        Ok(subagents)
    }

    fn collect_workspace_matching_sessions(
        &self,
        fields: Fields,
        provider: Option<&str>,
        workspace: &str,
        excluded_subagents: Option<Arc<HashSet<Uuid>>>,
    ) -> Result<HashMap<Uuid, EventDocAnchor>> {
        let workspace_query = literal_fact_union_contains_query(
            [
                fields.fact_workspace,
                fields.fact_session_cwd,
                fields.fact_tool_workdir,
                fields.fact_project,
            ],
            "workspace",
            workspace,
        )?;
        let query: Box<dyn Query> = match provider {
            Some(provider) => Box::new(BooleanQuery::new(vec![
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(fields.provider, provider),
                        IndexRecordOption::Basic,
                    )),
                ),
                (Occur::Must, workspace_query),
            ])),
            None => workspace_query,
        };
        Ok(self.searcher.search(
            query.as_ref(),
            &LatestSessionDocCollector {
                excluded_sessions: excluded_subagents,
            },
        )?)
    }

    fn collect_whole_session_aggregates(
        &self,
        fields: Fields,
        session_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, SessionFastAggregate>> {
        let query = TermSetQuery::new(
            session_ids
                .iter()
                .map(|session_id| Term::from_field_text(fields.session_id, &session_id.to_string()))
                .collect::<Vec<_>>(),
        );
        Ok(self.searcher.search(
            &query,
            &SessionActivityCollector {
                allowed_sessions: None,
                excluded_sessions: None,
            },
        )?)
    }

    fn hydrate_winning_session(
        &self,
        expected_session_id: Uuid,
        aggregate: SessionFastAggregate,
        workspace_doc: Option<DocAddress>,
        fields: Fields,
    ) -> Result<HydratedSessionSummary> {
        let primary_record =
            stored_core_event_record(&self.searcher, aggregate.last_event.address, fields)?;
        if primary_record.session_id.as_uuid() != expected_session_id {
            return Err(IndexError::InvalidStoredDocumentField("session_id"));
        }
        let stable_session_id = primary_record.session_id;
        let source_owner = primary_record.source.identity();
        let (provider_key, source_id) = primary_record
            .event
            .custom_source_identity()
            .map_or((None, None), |(key, id)| {
                (Some(key.to_owned()), Some(id.to_owned()))
            });
        let mut provider_session_id = primary_record.provider_session_id.clone();
        let mut parent_session_id = primary_record.parent_session_id;
        let mut root_session_id = primary_record.root_session_id;
        let mut facts = ExtractedSessionFacts::default();
        facts.extract_from(&primary_record.core_record);

        let mut extra_addresses = Vec::with_capacity(2);
        if aggregate.first_event.address != aggregate.last_event.address {
            extra_addresses.push(aggregate.first_event.address);
        }
        if let Some(workspace_address) = workspace_doc {
            if workspace_address != aggregate.last_event.address
                && workspace_address != aggregate.first_event.address
            {
                extra_addresses.push(workspace_address);
            }
        }

        for address in extra_addresses {
            if facts.is_complete() && provider_session_id.is_some() {
                break;
            }
            let record = stored_core_event_record(&self.searcher, address, fields)?;
            if record.session_id != stable_session_id {
                return Err(IndexError::InvalidStoredDocumentField("session_id"));
            }
            if provider_session_id.is_none() {
                provider_session_id = record.provider_session_id.clone();
            }
            if parent_session_id.is_none() {
                parent_session_id = record.parent_session_id;
            }
            if root_session_id.is_none() {
                root_session_id = record.root_session_id;
            }
            facts.extract_from(&record.core_record);
        }

        Ok(HydratedSessionSummary {
            session_id: stable_session_id,
            source_owner,
            provider: primary_record.provider.clone(),
            provider_key,
            source_id,
            provider_session_id,
            parent_session_id,
            root_session_id,
            cwd: facts.resolved_cwd(),
            project: facts.project,
            branch: facts.branch,
            started_at_unix_ms: aggregate.started_at_unix_ms,
            last_activity_at_unix_ms: aggregate.last_activity_at_unix_ms,
            events: aggregate.events,
        })
    }
}

struct HydratedSessionSummary {
    session_id: StableEntityId,
    source_owner: StableEntityId,
    provider: String,
    provider_key: Option<String>,
    source_id: Option<String>,
    provider_session_id: Option<String>,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    cwd: Option<String>,
    project: Option<String>,
    branch: Option<String>,
    started_at_unix_ms: Option<i64>,
    last_activity_at_unix_ms: Option<i64>,
    events: u64,
}

#[derive(Default)]
struct ExtractedSessionFacts {
    session_cwd: Option<String>,
    workspace: Option<String>,
    tool_workdir: Option<String>,
    project: Option<String>,
    branch: Option<String>,
}

impl ExtractedSessionFacts {
    fn is_complete(&self) -> bool {
        self.session_cwd.is_some() && self.project.is_some() && self.branch.is_some()
    }

    fn extract_from(&mut self, record: &CoreRecord) {
        let Some(activity) = record.content.activity.as_ref() else {
            return;
        };
        for fact in &activity.facts {
            if fact.value.is_empty() {
                continue;
            }
            let slot = match fact.kind {
                LiteralFactKind::SessionCwd => &mut self.session_cwd,
                LiteralFactKind::Workspace => &mut self.workspace,
                LiteralFactKind::ToolWorkdir => &mut self.tool_workdir,
                LiteralFactKind::Project => &mut self.project,
                LiteralFactKind::Branch => &mut self.branch,
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(fact.value.clone());
            }
        }
    }

    fn resolved_cwd(&self) -> Option<String> {
        self.session_cwd
            .clone()
            .or_else(|| self.workspace.clone())
            .or_else(|| self.tool_workdir.clone())
    }
}

fn compare_ranked_sessions(
    left: &(Uuid, SessionFastAggregate),
    right: &(Uuid, SessionFastAggregate),
) -> Ordering {
    right
        .1
        .last_activity_at_unix_ms
        .cmp(&left.1.last_activity_at_unix_ms)
        .then_with(|| right.1.max_event_sequence.cmp(&left.1.max_event_sequence))
        .then_with(|| left.0.cmp(&right.0))
}

fn build_activity_query(
    fields: Fields,
    provider: Option<&str>,
    since_unix_ms: Option<i64>,
    workspace_matches: Option<&HashMap<Uuid, EventDocAnchor>>,
) -> Box<dyn Query> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    if let Some(provider) = provider {
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.provider, provider),
                IndexRecordOption::Basic,
            )),
        ));
    }
    if let Some(since_unix_ms) = since_unix_ms {
        clauses.push((
            Occur::Must,
            Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(
                    fields.occurred_at_unix_ms,
                    since_unix_ms,
                )),
                Bound::Unbounded,
            )),
        ));
    }
    if let Some(workspace_matches) = workspace_matches {
        if workspace_matches.len() <= WORKSPACE_TERM_SET_THRESHOLD {
            clauses.push((
                Occur::Must,
                Box::new(TermSetQuery::new(
                    workspace_matches
                        .keys()
                        .map(|session_id| {
                            Term::from_field_text(fields.session_id, &session_id.to_string())
                        })
                        .collect::<Vec<_>>(),
                )),
            ));
        }
    }
    if clauses.is_empty() {
        Box::new(AllQuery)
    } else {
        Box::new(BooleanQuery::new(clauses))
    }
}

fn validate_session_list_fast_fields(searcher: &tantivy::Searcher) -> Result<()> {
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

struct SessionIdSetCollector;

struct SessionIdSetSegmentCollector {
    high: tantivy::columnar::Column<u64>,
    low: tantivy::columnar::Column<u64>,
    session_ids: HashSet<Uuid>,
    invalid: bool,
}

struct SessionIdSetSegmentFruit {
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

struct LatestSessionDocCollector {
    excluded_sessions: Option<Arc<HashSet<Uuid>>>,
}

struct LatestSessionDocSegmentCollector {
    segment_ord: SegmentOrdinal,
    session_high: tantivy::columnar::Column<u64>,
    session_low: tantivy::columnar::Column<u64>,
    event_high: tantivy::columnar::Column<u64>,
    event_low: tantivy::columnar::Column<u64>,
    event_sequence: tantivy::columnar::Column<u64>,
    occurred_at_unix_ms: tantivy::columnar::Column<i64>,
    excluded_sessions: Option<Arc<HashSet<Uuid>>>,
    latest: HashMap<Uuid, EventDocAnchor>,
    invalid: bool,
}

struct LatestSessionDocSegmentFruit {
    latest: HashMap<Uuid, EventDocAnchor>,
    invalid: bool,
}

impl Collector for LatestSessionDocCollector {
    type Fruit = HashMap<Uuid, EventDocAnchor>;
    type Child = LatestSessionDocSegmentCollector;

    fn for_segment(
        &self,
        segment_local_id: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let fast = segment.fast_fields();
        Ok(LatestSessionDocSegmentCollector {
            segment_ord: segment_local_id,
            session_high: fast.u64(SESSION_ID_HIGH_FIELD)?,
            session_low: fast.u64(SESSION_ID_LOW_FIELD)?,
            event_high: fast.u64(EVENT_ID_HIGH_FIELD)?,
            event_low: fast.u64(EVENT_ID_LOW_FIELD)?,
            event_sequence: fast.u64(EVENT_SEQUENCE_FIELD)?,
            occurred_at_unix_ms: fast.i64(OCCURRED_AT_UNIX_MS_FIELD)?,
            excluded_sessions: self.excluded_sessions.clone(),
            latest: HashMap::new(),
            invalid: false,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<LatestSessionDocSegmentFruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut merged: HashMap<Uuid, EventDocAnchor> = HashMap::new();
        for fruit in segment_fruits {
            if fruit.invalid {
                return Err(tantivy::TantivyError::InvalidArgument(
                    "session list fast field is absent".to_owned(),
                ));
            }
            for (session_id, anchor) in fruit.latest {
                merged
                    .entry(session_id)
                    .and_modify(|existing| {
                        if anchor.is_later_than(*existing) {
                            *existing = anchor;
                        }
                    })
                    .or_insert(anchor);
            }
        }
        Ok(merged)
    }
}

impl SegmentCollector for LatestSessionDocSegmentCollector {
    type Fruit = LatestSessionDocSegmentFruit;

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
        let anchor = EventDocAnchor {
            occurred_at_unix_ms: self.occurred_at_unix_ms.first(doc),
            event_sequence,
            event_id: Uuid::from_u128((u128::from(event_high) << 64) | u128::from(event_low)),
            address: DocAddress::new(self.segment_ord, doc),
        };
        self.latest
            .entry(session_id)
            .and_modify(|existing| {
                if anchor.is_later_than(*existing) {
                    *existing = anchor;
                }
            })
            .or_insert(anchor);
    }

    fn harvest(self) -> Self::Fruit {
        LatestSessionDocSegmentFruit {
            latest: self.latest,
            invalid: self.invalid,
        }
    }
}

struct SessionActivityCollector {
    allowed_sessions: Option<Arc<HashMap<Uuid, EventDocAnchor>>>,
    excluded_sessions: Option<Arc<HashSet<Uuid>>>,
}

struct SessionActivitySegmentCollector {
    segment_ord: SegmentOrdinal,
    session_high: tantivy::columnar::Column<u64>,
    session_low: tantivy::columnar::Column<u64>,
    event_high: tantivy::columnar::Column<u64>,
    event_low: tantivy::columnar::Column<u64>,
    event_sequence: tantivy::columnar::Column<u64>,
    occurred_at_unix_ms: tantivy::columnar::Column<i64>,
    allowed_sessions: Option<Arc<HashMap<Uuid, EventDocAnchor>>>,
    excluded_sessions: Option<Arc<HashSet<Uuid>>>,
    aggregates: HashMap<Uuid, SessionFastAggregate>,
    invalid: bool,
    overflowed: bool,
}

struct SessionActivitySegmentFruit {
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
            .is_some_and(|allowed| !allowed.contains_key(&session_id))
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

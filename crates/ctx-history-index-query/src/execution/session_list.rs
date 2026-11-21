use std::{collections::HashSet, sync::Arc};

use ctx_history_index_format::SessionAuthorityKey;

use super::*;

mod collectors;
use collectors::{
    validate_session_list_fast_fields, EventDocAnchor, SessionActivityCollector,
    SessionFastAggregate, SessionIdSetCollector,
};

const WORKSPACE_TERM_SET_THRESHOLD: usize = 1_024;
const MISSING_CWD: u8 = 0b001;
const MISSING_PROJECT: u8 = 0b010;
const MISSING_BRANCH: u8 = 0b100;

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
                excluded_subagents.as_deref(),
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
                    allowed_sessions: workspace_matches,
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
            let partial = self.hydrate_winning_session(session_id, aggregate, fields)?;
            coordinates.push((partial.session_id, partial.source_owner));
            hydrated.push(partial);
        }

        let incomplete: HashMap<Uuid, (u8, DocAddress, DocAddress)> = hydrated
            .iter()
            .filter_map(|partial| {
                let mask = partial.facts.missing_fact_mask();
                (mask != 0).then_some((
                    partial.session_id.as_uuid(),
                    (
                        mask,
                        partial.last_event_address,
                        partial.first_event_address,
                    ),
                ))
            })
            .collect();
        if !incomplete.is_empty() {
            let missing_fact_docs = self.collect_missing_fact_docs(fields, &incomplete)?;
            for partial in &mut hydrated {
                if let Some(anchor) = missing_fact_docs.get(&partial.session_id.as_uuid()) {
                    partial.merge_address(&self.searcher, anchor.address, fields)?;
                }
            }
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
                cwd: partial.facts.resolved_cwd(),
                project: partial.facts.project,
                branch: partial.facts.branch,
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
                let claims = key.direct_claims()?;
                if claims.parent_session_id.is_none()
                    || matches!(
                        claims.relationship,
                        Some(
                            ProviderNativeSessionRelationship::Forked
                                | ProviderNativeSessionRelationship::ResumedFrom
                        )
                    )
                {
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
        excluded_subagents: Option<&HashSet<Uuid>>,
    ) -> Result<HashSet<Uuid>> {
        let needle = AsciiFoldSubstring::new(workspace)?;
        let workspace_fields = [
            fields.fact_workspace,
            fields.fact_session_cwd,
            fields.fact_tool_workdir,
            fields.fact_project,
        ];
        let mut matched = HashSet::new();
        for segment in self.searcher.segment_readers() {
            let provider_bitmap = if let Some(provider) = provider {
                let inverted = segment.inverted_index(fields.provider)?;
                let term = Term::from_field_text(fields.provider, provider);
                let Some(mut postings) = inverted.read_postings(&term, IndexRecordOption::Basic)?
                else {
                    continue;
                };
                let word_count = usize::try_from(segment.max_doc())
                    .map_err(|_| IndexError::CountOverflow)?
                    .div_ceil(u64::BITS as usize);
                let mut bits = vec![0_u64; word_count];
                let mut any = false;
                let mut doc = postings.doc();
                while doc != TERMINATED {
                    if !segment.is_deleted(doc) {
                        let idx = usize::try_from(doc).map_err(|_| IndexError::CountOverflow)?;
                        bits[idx / 64] |= 1_u64 << (idx % 64);
                        any = true;
                    }
                    doc = postings.advance();
                }
                if !any {
                    continue;
                }
                Some(bits)
            } else {
                None
            };

            let fast = segment.fast_fields();
            let session_high = fast.u64(SESSION_ID_HIGH_FIELD)?;
            let session_low = fast.u64(SESSION_ID_LOW_FIELD)?;

            for field in workspace_fields {
                let inverted = segment.inverted_index(field)?;
                let mut terms = inverted.terms().stream()?;
                while terms.advance() {
                    let key = terms.key();
                    std::str::from_utf8(key)
                        .map_err(|_| IndexError::InvalidStoredDocumentField("literal_fact"))?;
                    if !needle.matches(key) {
                        continue;
                    }
                    let mut postings = inverted
                        .read_postings_from_terminfo(terms.value(), IndexRecordOption::Basic)?;
                    let mut doc = postings.doc();
                    while doc != TERMINATED {
                        if !segment.is_deleted(doc)
                            && provider_bitmap
                                .as_deref()
                                .is_none_or(|bits| doc_bit_is_set(bits, doc))
                        {
                            let (Some(high), Some(low)) =
                                (session_high.first(doc), session_low.first(doc))
                            else {
                                return Err(IndexError::InvalidStoredDocumentField(
                                    SESSION_ID_HIGH_FIELD,
                                ));
                            };
                            let session_id =
                                Uuid::from_u128((u128::from(high) << 64) | u128::from(low));
                            if excluded_subagents
                                .is_none_or(|excluded| !excluded.contains(&session_id))
                            {
                                matched.insert(session_id);
                            }
                        }
                        doc = postings.advance();
                    }
                }
            }
        }
        Ok(matched)
    }

    fn collect_missing_fact_docs(
        &self,
        fields: Fields,
        incomplete: &HashMap<Uuid, (u8, DocAddress, DocAddress)>,
    ) -> Result<HashMap<Uuid, EventDocAnchor>> {
        let mut best: HashMap<Uuid, (u8, EventDocAnchor)> = HashMap::new();
        let fact_specs = [
            (fields.fact_session_cwd, MISSING_CWD, 5_u8),
            (fields.fact_workspace, MISSING_CWD, 4_u8),
            (fields.fact_tool_workdir, MISSING_CWD, 3_u8),
            (fields.fact_project, MISSING_PROJECT, 2_u8),
            (fields.fact_branch, MISSING_BRANCH, 1_u8),
        ];

        for (segment_ord, segment) in self.searcher.segment_readers().iter().enumerate() {
            let segment_ord =
                SegmentOrdinal::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
            let session_inverted = segment.inverted_index(fields.session_id)?;
            let mut candidates: Vec<(DocId, Uuid, u8)> = Vec::new();
            let mut segment_missing_union = 0_u8;

            for (&session_id, &(missing_mask, last_addr, first_addr)) in incomplete {
                let term = Term::from_field_text(fields.session_id, &session_id.to_string());
                let Some(mut postings) =
                    session_inverted.read_postings(&term, IndexRecordOption::Basic)?
                else {
                    continue;
                };
                let mut doc = postings.doc();
                while doc != TERMINATED {
                    if !segment.is_deleted(doc) {
                        let address = DocAddress::new(segment_ord, doc);
                        if address != last_addr && address != first_addr {
                            candidates.push((doc, session_id, missing_mask));
                            segment_missing_union |= missing_mask;
                        }
                    }
                    doc = postings.advance();
                }
            }
            if candidates.is_empty() {
                continue;
            }
            candidates.sort_unstable_by_key(|(doc_id, _, _)| *doc_id);

            let fast = segment.fast_fields();
            let event_high = fast.u64(EVENT_ID_HIGH_FIELD)?;
            let event_low = fast.u64(EVENT_ID_LOW_FIELD)?;
            let event_sequence = fast.u64(EVENT_SEQUENCE_FIELD)?;
            let occurred_at_unix_ms = fast.i64(OCCURRED_AT_UNIX_MS_FIELD)?;

            for (field, mask_bit, priority) in fact_specs {
                if segment_missing_union & mask_bit == 0 {
                    continue;
                }
                let field_candidates: Vec<(DocId, Uuid)> = candidates
                    .iter()
                    .filter_map(|&(doc_id, session_id, mask)| {
                        ((mask & mask_bit) != 0).then_some((doc_id, session_id))
                    })
                    .collect();
                if field_candidates.is_empty() {
                    continue;
                }
                let max_field_doc = field_candidates
                    .last()
                    .map(|(doc_id, _)| *doc_id)
                    .unwrap_or(0);
                let inverted = segment.inverted_index(field)?;
                let mut terms = inverted.terms().stream()?;
                while terms.advance() {
                    let mut postings = inverted
                        .read_postings_from_terminfo(terms.value(), IndexRecordOption::Basic)?;
                    let mut doc = postings.doc();
                    for &(target_doc, session_id) in &field_candidates {
                        if doc == TERMINATED || doc > max_field_doc {
                            break;
                        }
                        if doc < target_doc {
                            doc = postings.seek(target_doc);
                        }
                        if doc == target_doc {
                            let (Some(high), Some(low), Some(sequence)) = (
                                event_high.first(doc),
                                event_low.first(doc),
                                event_sequence.first(doc),
                            ) else {
                                return Err(IndexError::InvalidStoredDocumentField(
                                    EVENT_ID_HIGH_FIELD,
                                ));
                            };
                            let anchor = EventDocAnchor {
                                occurred_at_unix_ms: occurred_at_unix_ms.first(doc),
                                event_sequence: sequence,
                                event_id: Uuid::from_u128(
                                    (u128::from(high) << 64) | u128::from(low),
                                ),
                                address: DocAddress::new(segment_ord, doc),
                            };
                            best.entry(session_id)
                                .and_modify(|(best_prio, best_anchor)| {
                                    if priority > *best_prio
                                        || (priority == *best_prio
                                            && anchor.is_later_than(*best_anchor))
                                    {
                                        *best_prio = priority;
                                        *best_anchor = anchor;
                                    }
                                })
                                .or_insert((priority, anchor));
                        }
                    }
                }
            }
        }

        Ok(best
            .into_iter()
            .map(|(session_id, (_, anchor))| (session_id, anchor))
            .collect())
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
        let mut facts = ExtractedSessionFacts::default();
        facts.extract_from(&primary_record.core_record);

        let mut summary = HydratedSessionSummary {
            session_id: stable_session_id,
            source_owner,
            provider: primary_record.provider.clone(),
            provider_key,
            source_id,
            provider_session_id: primary_record.provider_session_id.clone(),
            parent_session_id: primary_record.parent_session_id,
            root_session_id: primary_record.root_session_id,
            facts,
            first_event_address: aggregate.first_event.address,
            last_event_address: aggregate.last_event.address,
            started_at_unix_ms: aggregate.started_at_unix_ms,
            last_activity_at_unix_ms: aggregate.last_activity_at_unix_ms,
            events: aggregate.events,
        };

        if aggregate.first_event.address != aggregate.last_event.address
            && !summary.is_metadata_complete()
        {
            summary.merge_address(&self.searcher, aggregate.first_event.address, fields)?;
        }

        Ok(summary)
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
    facts: ExtractedSessionFacts,
    first_event_address: DocAddress,
    last_event_address: DocAddress,
    started_at_unix_ms: Option<i64>,
    last_activity_at_unix_ms: Option<i64>,
    events: u64,
}

impl HydratedSessionSummary {
    fn is_metadata_complete(&self) -> bool {
        let custom_source_complete =
            self.provider != "custom" || (self.provider_key.is_some() && self.source_id.is_some());
        self.facts.is_complete() && self.provider_session_id.is_some() && custom_source_complete
    }

    fn merge_address(
        &mut self,
        searcher: &tantivy::Searcher,
        address: DocAddress,
        fields: Fields,
    ) -> Result<()> {
        let record = stored_core_event_record(searcher, address, fields)?;
        if record.session_id != self.session_id {
            return Err(IndexError::InvalidStoredDocumentField("session_id"));
        }
        if self.provider_key.is_none() || self.source_id.is_none() {
            if let Some((key, id)) = record.event.custom_source_identity() {
                if self.provider_key.is_none() {
                    self.provider_key = Some(key.to_owned());
                }
                if self.source_id.is_none() {
                    self.source_id = Some(id.to_owned());
                }
            }
        }
        if self.provider_session_id.is_none() {
            self.provider_session_id = record.provider_session_id.clone();
        }
        if self.parent_session_id.is_none() {
            self.parent_session_id = record.parent_session_id;
        }
        if self.root_session_id.is_none() {
            self.root_session_id = record.root_session_id;
        }
        self.facts.extract_from(&record.core_record);
        Ok(())
    }
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

    fn missing_fact_mask(&self) -> u8 {
        let mut mask = 0;
        if self.resolved_cwd().is_none() {
            mask |= MISSING_CWD;
        }
        if self.project.is_none() {
            mask |= MISSING_PROJECT;
        }
        if self.branch.is_none() {
            mask |= MISSING_BRANCH;
        }
        mask
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

fn doc_bit_is_set(bits: &[u64], doc: DocId) -> bool {
    let Ok(idx) = usize::try_from(doc) else {
        return false;
    };
    bits.get(idx / 64)
        .is_some_and(|word| (word & (1_u64 << (idx % 64))) != 0)
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
    workspace_matches: Option<&HashSet<Uuid>>,
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
                        .iter()
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

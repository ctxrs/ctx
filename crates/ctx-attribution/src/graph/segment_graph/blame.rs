//! Typed blame and commit-lineage reads over one pinned generation.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::protocol::{
    CommitLineageOperationKind, CommitLineageProofClass, CommitLineageRelationClass,
    CommitLineageState, GitObjectFormat,
};

use crate::protocol::{ProductionRelationship, ResourceKind};
use crate::query::commit_lineage::{
    CommitLineageGraph, OperationEvidenceIdentity, OperationFact, OperationFactKind,
    OperationGroupPage, OperationIdPage, OperationMetadata,
};
use crate::query::{
    BlameFactFamily, BlameFactPosition, Confidence, Fact, FactState, MAX_ATTRIBUTION_CANDIDATES,
    ProductionAttribution, QueryError, QueryPage, Resource, ResourceId,
};

use super::SegmentGraph;
use super::merge::{
    FactAssembly, Lookup, QueryWork, ScanControl, bounded_limit, graph_id, page_limit,
};
use crate::graph::segment::model::ServingResourceQueryExt as _;
use crate::graph::segment::{
    AttributeValue, FORGE_CLOSE, FORGE_COMMENT, FORGE_CREATE, FORGE_EDIT, FORGE_MERGE,
    FORGE_PULL_REQUEST_CONTAINS_COMMIT, FORGE_PULL_REQUEST_MERGED_AS,
    FORGE_PULL_REQUEST_REFERENCED, FORGE_REOPEN, FORGE_REVIEW, GIT_COMMIT_AMBIGUOUS,
    GIT_COMMIT_AMENDED, GIT_COMMIT_CHERRY_PICKED, GIT_COMMIT_INSPECTED, GIT_COMMIT_PRODUCED,
    GIT_COMMIT_PUSHED, GIT_COMMIT_REFERENCED, GIT_COMMIT_REPLACED, GIT_COMMIT_REVERTED,
    ServingFactState, ServingRecord,
};

const COMMIT_OPERATION_FAMILIES: [&str; 3] = [
    GIT_COMMIT_REPLACED,
    GIT_COMMIT_CHERRY_PICKED,
    GIT_COMMIT_PRODUCED,
];

const COMMIT_FAMILIES: [&str; 8] = [
    GIT_COMMIT_PRODUCED,
    GIT_COMMIT_AMBIGUOUS,
    GIT_COMMIT_AMENDED,
    GIT_COMMIT_CHERRY_PICKED,
    GIT_COMMIT_REVERTED,
    GIT_COMMIT_PUSHED,
    GIT_COMMIT_INSPECTED,
    GIT_COMMIT_REFERENCED,
];

const PULL_REQUEST_FAMILIES: [&str; 10] = [
    FORGE_PULL_REQUEST_MERGED_AS,
    FORGE_PULL_REQUEST_CONTAINS_COMMIT,
    FORGE_PULL_REQUEST_REFERENCED,
    FORGE_CREATE,
    FORGE_REVIEW,
    FORGE_COMMENT,
    FORGE_MERGE,
    FORGE_EDIT,
    FORGE_CLOSE,
    FORGE_REOPEN,
];

impl SegmentGraph {
    pub(crate) fn blame_position(
        &self,
        fact_id: &str,
        family: BlameFactFamily,
        target: &ResourceId,
    ) -> Result<Option<BlameFactPosition>, QueryError> {
        let families: &[&str] = match family {
            BlameFactFamily::Commit => &COMMIT_FAMILIES,
            BlameFactFamily::PullRequest => &PULL_REQUEST_FAMILIES,
        };
        let mut work = QueryWork::new();
        let mut fact = None::<FactAssembly>;
        self.scan_records_for_families_with_work(
            families,
            Lookup::Exact {
                repository: None,
                term: fact_id,
            },
            &mut work,
            &mut |record| {
                if record.record_id != fact_id
                    || !subject_is(&record, target)
                    || (record.fact_family.as_str() == GIT_COMMIT_PRODUCED
                        && !queryable_commit_production(&record))
                {
                    return Ok(ScanControl::Continue);
                }
                let candidate = self.assemble_record(&record)?;
                if !pull_request_proof(candidate.fact(), family) {
                    return Ok(ScanControl::Continue);
                }
                if let Some(stored) = fact.as_mut() {
                    stored.merge(candidate)?;
                } else {
                    fact = Some(candidate);
                }
                Ok(ScanControl::Continue)
            },
        )?;
        Ok(fact.map(|fact| position(fact.fact(), family)))
    }

    pub(super) fn facts_page(
        &self,
        target: &ResourceId,
        family: BlameFactFamily,
        after: Option<&BlameFactPosition>,
        limit: usize,
    ) -> Result<QueryPage<(Fact, BlameFactPosition), BlameFactPosition>, QueryError> {
        let mut work = QueryWork::new();
        self.facts_page_with_work(target, family, after, limit, &mut work)
    }

    pub(super) fn facts_page_with_work(
        &self,
        target: &ResourceId,
        family: BlameFactFamily,
        after: Option<&BlameFactPosition>,
        limit: usize,
        work: &mut QueryWork,
    ) -> Result<QueryPage<(Fact, BlameFactPosition), BlameFactPosition>, QueryError> {
        let limit = page_limit(limit);
        if limit == 0 {
            return Ok(QueryPage {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        let families: &[&str] = match family {
            BlameFactFamily::Commit => &COMMIT_FAMILIES,
            BlameFactFamily::PullRequest => &PULL_REQUEST_FAMILIES,
        };
        let retained_limit = limit.checked_add(1).ok_or_else(Self::query_error)?;
        let mut retained = BoundedFactPage::new(family, retained_limit);
        self.scan_records_for_families_with_work(
            families,
            Lookup::Exact {
                repository: None,
                term: &target.0,
            },
            work,
            &mut |record| {
                if !subject_is(&record, target) {
                    return Ok(ScanControl::Continue);
                }
                if record.fact_family.as_str() == GIT_COMMIT_PRODUCED
                    && !queryable_commit_production(&record)
                {
                    return Ok(ScanControl::Continue);
                }
                let fact = self.assemble_record(&record)?;
                if !pull_request_proof(fact.fact(), family) {
                    return Ok(ScanControl::Continue);
                }
                let fact_position = position(fact.fact(), family);
                if after.is_some_and(|after| !position_order(&fact_position, after, family).is_gt())
                {
                    return Ok(ScanControl::Continue);
                }
                retained.consider(fact, fact_position)?;
                Ok(ScanControl::Continue)
            },
        )?;
        work.observe_retained(retained.high_water());
        let mut rows = retained.into_rows()?;
        rows.sort_by(|left, right| position_order(&left.1, &right.1, family));
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = has_more
            .then(|| rows.last().map(|row| row.1.clone()))
            .flatten();
        Ok(QueryPage {
            items: rows,
            next_cursor,
        })
    }

    pub(super) fn attributions(
        &self,
        commit_ids: &[ResourceId],
        limit_per_commit: usize,
    ) -> Result<Vec<ProductionAttribution>, QueryError> {
        let commit_ids = commit_ids
            .iter()
            .take(bounded_limit(commit_ids.len()))
            .cloned()
            .collect::<BTreeSet<_>>();
        let limit_per_commit = bounded_limit(limit_per_commit);
        if commit_ids.is_empty() || limit_per_commit == 0 {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();
        let mut work = QueryWork::new();
        for commit in commit_ids {
            let records = self.records_for_families_with_work(
                &[GIT_COMMIT_PRODUCED, GIT_COMMIT_AMBIGUOUS],
                Lookup::Exact {
                    repository: None,
                    term: &commit.0,
                },
                &mut work,
            )?;
            let records = records
                .into_iter()
                .filter(|record| subject_is(record, &commit))
                // Authority comes only from exact production-family facts.
                // Reference, inspection, and copied records are never
                // reclassified merely because their chronology ranks highly.
                // A certified source-time command result may identify a
                // possible producer, but it never gains asserted authority.
                .filter(|record| match record.fact_family.as_str() {
                    GIT_COMMIT_PRODUCED => queryable_commit_production(record),
                    GIT_COMMIT_AMBIGUOUS => {
                        record.state == crate::graph::segment::ServingFactState::Ambiguous
                    }
                    _ => false,
                })
                .collect();
            let mut facts = self.assemble_facts(records)?;
            if facts.len() > limit_per_commit {
                return Err(QueryError::AttributionLimitExceeded(commit));
            }
            facts.sort_by(attribution_fact_order);
            let mut retained_sessions = BTreeSet::new();
            facts.retain(|fact| match fact.object.as_ref() {
                Some(session) => retained_sessions.insert(session.clone()),
                None => true,
            });
            facts.truncate(MAX_ATTRIBUTION_CANDIDATES);
            for fact in facts {
                let producing_session = fact.object.clone().ok_or_else(Self::query_error)?;
                let (relationship, confidence, state) = match (fact.fact_type.as_str(), fact.state)
                {
                    (GIT_COMMIT_PRODUCED, FactState::Asserted) => (
                        ProductionRelationship::ProducedBy,
                        fact.confidence,
                        fact.state,
                    ),
                    (GIT_COMMIT_PRODUCED | GIT_COMMIT_AMBIGUOUS, FactState::Ambiguous) => (
                        ProductionRelationship::PossiblyProducedBy,
                        Confidence::Ambiguous,
                        FactState::Ambiguous,
                    ),
                    _ => return Err(Self::query_error()),
                };
                output.push(ProductionAttribution {
                    fact_id: fact.id,
                    commit: fact.subject,
                    producing_session,
                    parent_session: None,
                    root_run: fact.root_run,
                    direct_actor: fact.direct_actor,
                    fact_occurred_at_ms: fact.occurred_at_ms,
                    relationship,
                    confidence,
                    state,
                    citations: fact.citations,
                });
            }
        }
        Ok(output)
    }
}

impl CommitLineageGraph for SegmentGraph {
    fn operation_ids_for_commit(
        &self,
        commit: &Resource,
        repository: &Resource,
        excluded: &BTreeSet<String>,
        limit: usize,
    ) -> Result<OperationIdPage, QueryError> {
        if limit == 0 {
            return Ok(OperationIdPage {
                operation_ids: Vec::new(),
                has_more: false,
            });
        }
        validate_lineage_lookup(commit, repository)?;
        let retained_limit = limit.checked_add(1).ok_or_else(Self::query_error)?;
        let mut work = QueryWork::new();
        let mut operation_ids = BTreeSet::new();
        let mut exceeded = false;
        self.scan_records_for_families_with_work(
            &COMMIT_OPERATION_FAMILIES,
            Lookup::Exact {
                repository: Some(&repository.display),
                term: &commit.display,
            },
            &mut work,
            &mut |record| {
                let Some(fact) = operation_fact(self, &record, repository)? else {
                    // Legacy production/replacement facts without the exact
                    // operation contract are not lineage evidence.
                    return Ok(ScanControl::Continue);
                };
                if !operation_fact_touches(&fact, commit) {
                    return Ok(ScanControl::Continue);
                }
                if excluded.contains(&fact.metadata.operation_id) {
                    return Ok(ScanControl::Continue);
                }
                operation_ids.insert(fact.metadata.operation_id);
                if operation_ids.len() > retained_limit {
                    operation_ids.pop_last();
                    exceeded = true;
                }
                Ok(ScanControl::Continue)
            },
        )?;
        let has_more = exceeded || operation_ids.len() > limit;
        while operation_ids.len() > limit {
            operation_ids.pop_last();
        }
        work.observe_retained(operation_ids.len());
        Ok(OperationIdPage {
            operation_ids: operation_ids.into_iter().collect(),
            has_more,
        })
    }

    fn operation_facts_for_id(
        &self,
        operation_id: &str,
        repository: &Resource,
        limit: usize,
    ) -> Result<OperationGroupPage, QueryError> {
        validate_lineage_repository(repository)?;
        if limit == 0 || !is_lower_sha256(operation_id) {
            return Err(Self::query_error());
        }
        let mut work = QueryWork::new();
        let mut facts = Vec::new();
        let control = self.scan_records_for_families_with_work(
            &COMMIT_OPERATION_FAMILIES,
            Lookup::Exact {
                repository: Some(&repository.display),
                term: operation_id,
            },
            &mut work,
            &mut |record| {
                let fact =
                    operation_fact(self, &record, repository)?.ok_or_else(Self::query_error)?;
                if fact.metadata.operation_id != operation_id {
                    return Err(Self::query_error());
                }
                facts.push(fact);
                Ok(if facts.len() > limit {
                    ScanControl::Stop
                } else {
                    ScanControl::Continue
                })
            },
        )?;
        let has_more = matches!(control, ScanControl::Stop);
        facts.truncate(limit);
        work.observe_retained(facts.len());
        Ok(OperationGroupPage { facts, has_more })
    }
}

fn validate_lineage_lookup(commit: &Resource, repository: &Resource) -> Result<(), QueryError> {
    validate_lineage_repository(repository)?;
    if commit.kind != ResourceKind::Commit
        || commit.logical_repository.as_ref() != Some(&repository.id)
        || git_object_format(&commit.display).is_none()
    {
        return Err(SegmentGraph::query_error());
    }
    Ok(())
}

fn validate_lineage_repository(repository: &Resource) -> Result<(), QueryError> {
    let graph_id = crate::graph::segment::logical_repository_graph_id(&repository.display)
        .map_err(|_| SegmentGraph::query_error())?;
    if repository.kind != ResourceKind::Repository || repository.id.0 != graph_id {
        return Err(SegmentGraph::query_error());
    }
    Ok(())
}

fn operation_fact(
    graph: &SegmentGraph,
    record: &ServingRecord,
    repository: &Resource,
) -> Result<Option<OperationFact>, QueryError> {
    if record.repository_id != repository.display {
        return Ok(None);
    }
    let Some(verified_operation_id) = record.verified_commit_operation_id() else {
        return Ok(None);
    };
    if attribute(record, "operation_id") != Some(verified_operation_id) {
        return Err(SegmentGraph::query_error());
    }
    let direct_session = record
        .direct_actor
        .as_ref()
        .ok_or_else(SegmentGraph::query_error)?;
    let root_matches = match (&record.scope, &record.event_owner.root_session_id) {
        (None, None) => true,
        (Some(scope), Some(root)) => {
            scope.typed_kind().ok() == Some(ResourceKind::Run)
                && scope.display().as_deref() == Ok(root.as_str())
        }
        _ => false,
    };
    if direct_session.typed_kind().ok() != Some(ResourceKind::Session)
        || direct_session.display().as_deref() != Ok(record.event_owner.direct_session_id.as_str())
        || !root_matches
    {
        return Err(SegmentGraph::query_error());
    }
    let operation_id = verified_operation_id;
    let Some(receipt_id) = attribute(record, "receipt_id") else {
        return Ok(None);
    };
    if !is_lower_sha256(operation_id) || !is_lower_sha256(receipt_id) {
        return Err(SegmentGraph::query_error());
    }
    let kind = match attribute(record, "operation_kind") {
        Some("amend") => CommitLineageOperationKind::Amend,
        Some("rebase") => CommitLineageOperationKind::Rebase,
        Some("cherry_pick") => CommitLineageOperationKind::CherryPick,
        _ => return Err(SegmentGraph::query_error()),
    };
    let relation_class = match attribute(record, "relation_class") {
        Some("replacement") => CommitLineageRelationClass::Replacement,
        Some("derivation") => CommitLineageRelationClass::Derivation,
        _ => return Err(SegmentGraph::query_error()),
    };
    if attribute(record, "proof_class") != Some("repository_verified")
        || attribute(record, "operation_state") != Some("asserted")
    {
        return Err(SegmentGraph::query_error());
    }
    let object_format = match attribute(record, "object_format") {
        Some("sha1") => GitObjectFormat::Sha1,
        Some("sha256") => GitObjectFormat::Sha256,
        _ => return Err(SegmentGraph::query_error()),
    };
    if record.occurred_at_unix_ms.is_some_and(|value| value < 0) {
        return Err(SegmentGraph::query_error());
    }
    let metadata = OperationMetadata {
        operation_id: operation_id.to_owned(),
        receipt_id: receipt_id.to_owned(),
        kind,
        relation_class,
        proof_class: CommitLineageProofClass::RepositoryVerified,
        state: CommitLineageState::Asserted,
        object_format,
        observed_at_ms: record.occurred_at_unix_ms,
        evidence_identity: OperationEvidenceIdentity {
            source_id: record.event_owner.source_id.clone(),
            event_id: record.event_owner.event_id.clone(),
            direct_session_id: record.event_owner.direct_session_id.clone(),
            root_session_id: record.event_owner.root_session_id.clone(),
            event_sequence: record.event_owner.event_sequence,
        },
    };
    let subject = record
        .subject
        .to_query_resource()
        .map_err(|_| SegmentGraph::query_error())?;
    let object = record
        .object
        .as_ref()
        .ok_or_else(SegmentGraph::query_error)?
        .to_query_resource()
        .map_err(|_| SegmentGraph::query_error())?;
    let predicate = attribute(record, "_ctx.predicate");
    let kind = match record.fact_family.as_str() {
        GIT_COMMIT_REPLACED if predicate == Some("replaces") => OperationFactKind::Mapping {
            source: object,
            result: subject,
        },
        GIT_COMMIT_CHERRY_PICKED if predicate == Some("cherry_picked_from") => {
            OperationFactKind::Mapping {
                source: object,
                result: subject,
            }
        }
        GIT_COMMIT_PRODUCED if predicate == Some("produced_by") => {
            if object.kind != ResourceKind::Session
                || graph_id(direct_session).as_deref() != Ok(object.id.0.as_str())
            {
                return Err(SegmentGraph::query_error());
            }
            OperationFactKind::Yield {
                yield_id: record.record_id.clone(),
                result: subject,
                actor: object,
            }
        }
        _ => return Err(SegmentGraph::query_error()),
    };
    Ok(Some(OperationFact {
        metadata,
        kind,
        citations: graph.active_citations(&record.citations)?,
    }))
}

fn attribute<'a>(record: &'a ServingRecord, name: &str) -> Option<&'a str> {
    match record.attributes.get(name) {
        Some(AttributeValue::String(value)) => Some(value),
        _ => None,
    }
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn git_object_format(value: &str) -> Option<GitObjectFormat> {
    let format = match value.len() {
        40 => GitObjectFormat::Sha1,
        64 => GitObjectFormat::Sha256,
        _ => return None,
    };
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        .then_some(format)
}

fn operation_fact_touches(fact: &OperationFact, commit: &Resource) -> bool {
    match &fact.kind {
        OperationFactKind::Mapping { source, result } => {
            source.id == commit.id || result.id == commit.id
        }
        OperationFactKind::Yield { result, .. } => result.id == commit.id,
    }
}

struct BoundedFactPage {
    family: BlameFactFamily,
    limit: usize,
    high_water: usize,
    rows: BTreeMap<String, (FactAssembly, BlameFactPosition)>,
}

impl BoundedFactPage {
    fn new(family: BlameFactFamily, limit: usize) -> Self {
        Self {
            family,
            limit,
            high_water: 0,
            rows: BTreeMap::new(),
        }
    }

    fn consider(
        &mut self,
        fact: FactAssembly,
        position: BlameFactPosition,
    ) -> Result<(), QueryError> {
        let fact_id = fact.fact().id.clone();
        if let Some((stored, stored_position)) = self.rows.get_mut(&fact_id) {
            if stored_position != &position {
                return Err(SegmentGraph::query_error());
            }
            stored.merge(fact)?;
            return Ok(());
        }

        if self.rows.len() == self.limit {
            let worst_id = self
                .rows
                .iter()
                .max_by(|left, right| position_order(&left.1.1, &right.1.1, self.family))
                .map(|(id, _)| id.clone())
                .ok_or_else(SegmentGraph::query_error)?;
            let is_better = self
                .rows
                .get(&worst_id)
                .is_some_and(|(_, worst)| position_order(&position, worst, self.family).is_lt());
            if !is_better {
                return Ok(());
            }
            self.rows.remove(&worst_id);
        }

        self.rows.insert(fact_id, (fact, position));
        self.high_water = self.high_water.max(self.rows.len());
        Ok(())
    }

    const fn high_water(&self) -> usize {
        self.high_water
    }

    fn into_rows(self) -> Result<Vec<(Fact, BlameFactPosition)>, QueryError> {
        self.rows
            .into_values()
            .map(|(fact, position)| Ok((fact.into_fact()?, position)))
            .collect()
    }
}

fn subject_is(record: &ServingRecord, target: &ResourceId) -> bool {
    graph_id(&record.subject).as_deref() == Ok(target.0.as_str())
}

fn pull_request_proof(fact: &Fact, family: BlameFactFamily) -> bool {
    if family != BlameFactFamily::PullRequest
        || !matches!(
            fact.fact_type.as_str(),
            FORGE_PULL_REQUEST_MERGED_AS | FORGE_PULL_REQUEST_CONTAINS_COMMIT
        )
    {
        return true;
    }
    fact.state == FactState::Asserted && fact.confidence == crate::query::Confidence::Verified
}

fn position(fact: &Fact, family: BlameFactFamily) -> BlameFactPosition {
    let class = match family {
        BlameFactFamily::Commit => 0,
        BlameFactFamily::PullRequest
            if matches!(
                fact.fact_type.as_str(),
                FORGE_PULL_REQUEST_MERGED_AS | FORGE_PULL_REQUEST_CONTAINS_COMMIT
            ) =>
        {
            0
        }
        BlameFactFamily::PullRequest => 1,
    };
    BlameFactPosition {
        class,
        rank: rank(&fact.fact_type),
        occurred_at_ms: fact.occurred_at_ms,
        resource_id: fact
            .object
            .as_ref()
            .map_or_else(String::new, |id| id.0.clone()),
        fact_id: fact.id.clone(),
    }
}

fn rank(family: &str) -> u8 {
    match family {
        GIT_COMMIT_PRODUCED | FORGE_PULL_REQUEST_MERGED_AS | FORGE_CREATE => 0,
        GIT_COMMIT_AMBIGUOUS | FORGE_PULL_REQUEST_CONTAINS_COMMIT | FORGE_REVIEW => 1,
        GIT_COMMIT_AMENDED | GIT_COMMIT_CHERRY_PICKED | GIT_COMMIT_REVERTED | FORGE_COMMENT => 2,
        GIT_COMMIT_PUSHED | FORGE_MERGE => 3,
        GIT_COMMIT_INSPECTED | FORGE_EDIT => 4,
        FORGE_CLOSE => 5,
        FORGE_REOPEN => 6,
        GIT_COMMIT_REFERENCED => 5,
        FORGE_PULL_REQUEST_REFERENCED => 7,
        _ => u8::MAX,
    }
}

fn position_order(
    left: &BlameFactPosition,
    right: &BlameFactPosition,
    family: BlameFactFamily,
) -> Ordering {
    left.class.cmp(&right.class).then_with(|| match family {
        BlameFactFamily::Commit => left
            .rank
            .cmp(&right.rank)
            .then_with(|| optional_time_desc(left.occurred_at_ms, right.occurred_at_ms))
            .then_with(|| left.resource_id.cmp(&right.resource_id))
            .then_with(|| left.fact_id.cmp(&right.fact_id)),
        BlameFactFamily::PullRequest if left.class == 0 => left
            .rank
            .cmp(&right.rank)
            .then_with(|| left.resource_id.cmp(&right.resource_id))
            .then_with(|| optional_time_desc(left.occurred_at_ms, right.occurred_at_ms))
            .then_with(|| left.fact_id.cmp(&right.fact_id)),
        BlameFactFamily::PullRequest => {
            optional_time_desc(left.occurred_at_ms, right.occurred_at_ms)
                .then_with(|| left.rank.cmp(&right.rank))
                .then_with(|| left.resource_id.cmp(&right.resource_id))
                .then_with(|| left.fact_id.cmp(&right.fact_id))
        }
    })
}

fn optional_time_desc(left: Option<i64>, right: Option<i64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn queryable_commit_production(record: &ServingRecord) -> bool {
    record.state == ServingFactState::Asserted || record.is_possible_commit_production_evidence()
}

fn attribution_fact_order(left: &Fact, right: &Fact) -> Ordering {
    attribution_rank(left)
        .cmp(&attribution_rank(right))
        .then_with(|| left.object.cmp(&right.object))
        .then_with(|| left.id.cmp(&right.id))
}

fn attribution_rank(fact: &Fact) -> u8 {
    if fact.fact_type == GIT_COMMIT_PRODUCED {
        0
    } else {
        1
    }
}

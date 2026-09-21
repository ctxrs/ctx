//! Bounded exact-object commit-operation lineage composition.
//!
//! This retained query path traverses only typed Core commit-operation mappings.
//! It never reconstructs session ancestry, substitutes the requested object, or
//! transfers an actor from one yielded result to another.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::protocol::{
    CommitLineageOperationKind, CommitLineageProofClass, CommitLineageRelationClass,
    CommitLineageState, GitObjectFormat,
};

use crate::protocol::MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS;
use crate::query::{Citation, QueryError, Resource, ResourceId, ResourceKind};

/// Public protocol bounds for one commit-lineage result.
pub(crate) const MAX_RETURNED_OPERATIONS: usize =
    crate::protocol::MAX_COMMIT_LINEAGE_RETURNED_EVENTS as usize;
pub(crate) const MAX_EXAMINED_OPERATIONS: usize =
    crate::protocol::MAX_COMMIT_LINEAGE_EXAMINED_EVENTS as usize;
/// A complete operation group is separately bounded from the distinct-operation
/// traversal budget. This is intentionally a fact bound, not an event count.
pub(crate) const MAX_FACTS_PER_OPERATION: usize = MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS * 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExactCommit {
    pub(crate) resource: Resource,
    /// Kept explicitly until the public repository-identity carrier is pinned.
    pub(crate) repository: Resource,
    pub(crate) object_format: GitObjectFormat,
    pub(crate) oid: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationMetadata {
    pub(crate) operation_id: String,
    pub(crate) receipt_id: String,
    pub(crate) kind: CommitLineageOperationKind,
    pub(crate) relation_class: CommitLineageRelationClass,
    pub(crate) proof_class: CommitLineageProofClass,
    pub(crate) state: CommitLineageState,
    pub(crate) object_format: GitObjectFormat,
    pub(crate) observed_at_ms: Option<i64>,
    pub(crate) evidence_identity: OperationEvidenceIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationEvidenceIdentity {
    pub(crate) source_id: String,
    pub(crate) event_id: String,
    pub(crate) direct_session_id: String,
    pub(crate) root_session_id: Option<String>,
    pub(crate) event_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OperationFactKind {
    Mapping {
        source: Resource,
        result: Resource,
    },
    Yield {
        yield_id: String,
        result: Resource,
        actor: Resource,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationFact {
    pub(crate) metadata: OperationMetadata,
    pub(crate) kind: OperationFactKind,
    pub(crate) citations: Vec<Citation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationIdPage {
    pub(crate) operation_ids: Vec<String>,
    /// More distinct adjacent operations exist beyond the requested limit.
    pub(crate) has_more: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationGroupPage {
    pub(crate) facts: Vec<OperationFact>,
    /// The one exact operation group exceeded its independent fact bound.
    pub(crate) has_more: bool,
}

/// Query-only reader contract. Implementations must use the exact full object
/// ID and exact logical repository supplied by the composer.
pub(crate) trait CommitLineageGraph {
    fn operation_ids_for_commit(
        &self,
        commit: &Resource,
        repository: &Resource,
        excluded: &BTreeSet<String>,
        limit: usize,
    ) -> Result<OperationIdPage, QueryError>;

    fn operation_facts_for_id(
        &self,
        operation_id: &str,
        repository: &Resource,
        limit: usize,
    ) -> Result<OperationGroupPage, QueryError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LineageEdge {
    pub(crate) metadata: OperationMetadata,
    pub(crate) source: ExactCommit,
    pub(crate) result: ExactCommit,
    pub(crate) actor: Resource,
    pub(crate) observed_at_ms: Option<i64>,
    pub(crate) citations: Vec<Citation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LineageYield {
    pub(crate) operation_id: String,
    pub(crate) yield_id: String,
    pub(crate) actor: Resource,
    pub(crate) proof_class: CommitLineageProofClass,
    pub(crate) state: CommitLineageState,
    pub(crate) observed_at_ms: Option<i64>,
    pub(crate) citations: Vec<Citation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LineageTruncation {
    ReturnedOperationLimit,
    ExaminedOperationLimit,
    EvidenceGap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub struct CommitLineageDraft {
    pub(crate) requested: ExactCommit,
    pub(crate) edges: Vec<LineageEdge>,
    pub(crate) yielded_by: Vec<LineageYield>,
    pub(crate) origin: Option<ExactCommit>,
    pub(crate) complete: bool,
    pub(crate) ambiguous: bool,
    pub(crate) returned_operations: usize,
    pub(crate) examined_operations: usize,
    pub(crate) omitted_at_least: usize,
    pub(crate) truncation: Option<LineageTruncation>,
}

#[derive(Default)]
struct OperationGroup {
    metadata: Option<OperationMetadata>,
    inconsistent: bool,
    mappings: BTreeMap<(ResourceId, ResourceId), MappingFact>,
    yields: BTreeMap<ResourceId, Vec<YieldFact>>,
}

#[derive(Clone)]
struct MappingFact {
    source: Resource,
    result: Resource,
    citations: Vec<Citation>,
}

#[derive(Clone)]
struct YieldFact {
    yield_id: String,
    actor: Resource,
    citations: Vec<Citation>,
}

/// Composes the connected operation component containing `requested`.
///
/// The requested exact object remains the target even when an incoming
/// replacement operation is present.
pub(crate) fn compose<G: CommitLineageGraph>(
    graph: &G,
    requested: Resource,
    repository: Resource,
) -> Result<Option<CommitLineageDraft>, QueryError> {
    validate_requested(&requested, &repository)?;
    let requested_exact =
        exact_commit(&requested, &repository, object_format(&requested.display)?)?;

    let mut queue = VecDeque::from([requested.clone()]);
    let mut queued = BTreeSet::from([requested.id.clone()]);
    let mut visited = BTreeSet::new();
    let mut examined = BTreeSet::<String>::new();
    let mut retained = BTreeSet::<String>::new();
    let mut groups = BTreeMap::<String, OperationGroup>::new();
    let mut truncation = None;
    let mut omitted_at_least = 0_usize;

    'traversal: while let Some(current) = queue.pop_front() {
        if !visited.insert(current.id.clone()) {
            continue;
        }
        let remaining = MAX_EXAMINED_OPERATIONS.saturating_sub(examined.len());
        if remaining == 0 {
            truncation = Some(LineageTruncation::ExaminedOperationLimit);
            omitted_at_least = omitted_at_least.max(1);
            break;
        }
        let mut page =
            graph.operation_ids_for_commit(&current, &repository, &examined, remaining)?;
        page.operation_ids.sort();
        page.operation_ids.dedup();
        for operation_id in page.operation_ids {
            if groups.contains_key(&operation_id) {
                continue;
            }
            if examined.len() == MAX_EXAMINED_OPERATIONS {
                truncation = Some(LineageTruncation::ExaminedOperationLimit);
                omitted_at_least = omitted_at_least.max(1);
                break 'traversal;
            }
            examined.insert(operation_id.clone());
            let mut operation = graph.operation_facts_for_id(
                &operation_id,
                &repository,
                MAX_FACTS_PER_OPERATION,
            )?;
            if operation.has_more {
                return Err(QueryError::Backend(
                    "commit operation group exceeds its query fact bound".to_owned(),
                ));
            }
            operation.facts.sort_by(operation_fact_order);
            if !operation
                .facts
                .iter()
                .any(|fact| fact_touches(fact, &current))
            {
                return Err(QueryError::Backend(
                    "commit operation index lost its exact-object membership".to_owned(),
                ));
            }
            let mut group = OperationGroup::default();
            for fact in operation.facts {
                if fact.metadata.operation_id != operation_id
                    || !fact_in_repository(&fact, &repository)
                    || !metadata_matches_shape(&fact.metadata)
                {
                    group.inconsistent = true;
                    continue;
                }
                group.add(fact);
            }
            if group.has_returnable_event(&requested) {
                if retained.len() == MAX_RETURNED_OPERATIONS {
                    truncation = Some(LineageTruncation::ReturnedOperationLimit);
                    omitted_at_least = omitted_at_least.saturating_add(1);
                    break 'traversal;
                }
                retained.insert(operation_id.clone());
                group.enqueue_resources(&mut queue, &mut queued);
            }
            groups.insert(operation_id, group);
        }
        if page.has_more {
            truncation = Some(LineageTruncation::ExaminedOperationLimit);
            omitted_at_least = omitted_at_least.max(1);
            break;
        }
    }

    let mut ambiguous = false;
    let mut edges = Vec::new();
    let mut standalone = Vec::new();
    for (operation_id, group) in groups {
        let Some(ref metadata) = group.metadata else {
            mark_evidence_gap(&mut truncation, &mut omitted_at_least);
            ambiguous = true;
            continue;
        };
        if group.inconsistent {
            mark_evidence_gap(&mut truncation, &mut omitted_at_least);
            ambiguous = true;
            continue;
        }
        let operation_citations = group.citations();
        let mut operation_evidence_gap = false;
        for mapping in group.mappings.values() {
            let Some(yielded) = one_exact_yield(group.yields.get(&mapping.result.id)) else {
                operation_evidence_gap = true;
                ambiguous = true;
                continue;
            };
            edges.push(LineageEdge {
                metadata: metadata.clone(),
                source: exact_commit(&mapping.source, &repository, metadata.object_format)?,
                result: exact_commit(&mapping.result, &repository, metadata.object_format)?,
                actor: yielded.actor.clone(),
                observed_at_ms: metadata.observed_at_ms,
                citations: operation_citations.clone(),
            });
        }
        if operation_evidence_gap {
            mark_evidence_gap(&mut truncation, &mut omitted_at_least);
        }
        if !group
            .mappings
            .values()
            .any(|mapping| mapping.result.id == requested.id)
            && let Some(yields) = group.yields.get(&requested.id)
        {
            for yielded in yields {
                standalone.push(LineageYield {
                    operation_id: operation_id.clone(),
                    yield_id: yielded.yield_id.clone(),
                    actor: yielded.actor.clone(),
                    proof_class: metadata.proof_class,
                    state: metadata.state,
                    observed_at_ms: metadata.observed_at_ms,
                    citations: operation_citations.clone(),
                });
            }
        }
    }

    edges.sort_by(|left, right| {
        (
            left.metadata.operation_id.as_str(),
            left.metadata.kind,
            left.source.repository.display.as_str(),
            left.source.object_format,
            left.source.oid.as_str(),
            left.result.object_format,
            left.result.oid.as_str(),
        )
            .cmp(&(
                right.metadata.operation_id.as_str(),
                right.metadata.kind,
                right.source.repository.display.as_str(),
                right.source.object_format,
                right.source.oid.as_str(),
                right.result.object_format,
                right.result.oid.as_str(),
            ))
    });
    standalone.sort_by(|left, right| {
        (
            left.operation_id.as_str(),
            left.yield_id.as_str(),
            left.actor.id.0.as_str(),
        )
            .cmp(&(
                right.operation_id.as_str(),
                right.yield_id.as_str(),
                right.actor.id.0.as_str(),
            ))
    });

    if edges.is_empty() && standalone.is_empty() && !ambiguous && truncation.is_none() {
        return Ok(None);
    }
    let complete = truncation.is_none();
    let origin = if complete && !ambiguous {
        match unique_origin(&requested_exact, &edges) {
            UniqueOrigin::Reachable(origin) => Some(origin),
            UniqueOrigin::Unreachable => {
                ambiguous = true;
                None
            }
            UniqueOrigin::Ambiguous => {
                ambiguous = true;
                None
            }
            UniqueOrigin::Absent => None,
        }
    } else {
        None
    };
    let returned_operations = edges
        .iter()
        .map(|edge| edge.metadata.operation_id.as_str())
        .chain(
            standalone
                .iter()
                .map(|yielded| yielded.operation_id.as_str()),
        )
        .collect::<BTreeSet<_>>()
        .len();
    Ok(Some(CommitLineageDraft {
        requested: requested_exact,
        edges,
        yielded_by: standalone,
        origin,
        complete,
        ambiguous,
        returned_operations,
        examined_operations: examined.len(),
        omitted_at_least,
        truncation,
    }))
}

fn mark_evidence_gap(truncation: &mut Option<LineageTruncation>, omitted_at_least: &mut usize) {
    if truncation.is_none() {
        *truncation = Some(LineageTruncation::EvidenceGap);
    }
    *omitted_at_least = omitted_at_least.saturating_add(1);
}

impl OperationGroup {
    fn has_returnable_event(&self, requested: &Resource) -> bool {
        if self.inconsistent || self.metadata.is_none() {
            return false;
        }
        if self.mappings.is_empty() {
            return self
                .yields
                .get(&requested.id)
                .is_some_and(|yields| !yields.is_empty());
        }
        self.mappings
            .values()
            .all(|mapping| one_exact_yield(self.yields.get(&mapping.result.id)).is_some())
            && self.yields.keys().all(|result_id| {
                self.mappings
                    .values()
                    .any(|mapping| mapping.result.id == *result_id)
            })
    }

    fn enqueue_resources(&self, queue: &mut VecDeque<Resource>, queued: &mut BTreeSet<ResourceId>) {
        for commit in self
            .mappings
            .values()
            .flat_map(|mapping| [&mapping.source, &mapping.result])
            .chain(self.yields.keys().filter_map(|result_id| {
                self.mappings.values().find_map(|mapping| {
                    (mapping.result.id == *result_id).then_some(&mapping.result)
                })
            }))
        {
            if queued.insert(commit.id.clone()) {
                queue.push_back(commit.clone());
            }
        }
    }

    fn citations(&self) -> Vec<Citation> {
        self.mappings
            .values()
            .map(|mapping| mapping.citations.as_slice())
            .chain(
                self.yields
                    .values()
                    .flatten()
                    .map(|yielded| yielded.citations.as_slice()),
            )
            .fold(Vec::new(), |merged, citations| {
                merged_citations(&merged, citations)
            })
    }

    fn add(&mut self, fact: OperationFact) {
        if self
            .metadata
            .as_ref()
            .is_some_and(|stored| stored != &fact.metadata)
        {
            self.inconsistent = true;
            return;
        }
        self.metadata.get_or_insert_with(|| fact.metadata.clone());
        match fact.kind {
            OperationFactKind::Mapping { source, result } => {
                let key = (source.id.clone(), result.id.clone());
                let candidate = MappingFact {
                    source,
                    result,
                    citations: fact.citations,
                };
                if self
                    .mappings
                    .get(&key)
                    .is_some_and(|stored| stored.citations != candidate.citations)
                {
                    self.inconsistent = true;
                } else {
                    self.mappings.entry(key).or_insert(candidate);
                }
            }
            OperationFactKind::Yield {
                yield_id,
                result,
                actor,
            } => {
                if self
                    .yields
                    .values()
                    .flatten()
                    .any(|yielded| yielded.actor != actor)
                {
                    self.inconsistent = true;
                    return;
                }
                let yields = self.yields.entry(result.id.clone()).or_default();
                if !yields.iter().any(|stored| stored.yield_id == yield_id) {
                    yields.push(YieldFact {
                        yield_id,
                        actor,
                        citations: fact.citations,
                    });
                }
            }
        }
    }
}

fn validate_requested(requested: &Resource, repository: &Resource) -> Result<(), QueryError> {
    if requested.kind != ResourceKind::Commit
        || repository.kind != ResourceKind::Repository
        || requested.logical_repository.as_ref() != Some(&repository.id)
    {
        return Err(QueryError::Backend(
            "commit lineage target lost its exact logical repository".to_owned(),
        ));
    }
    object_format(&requested.display).map(|_| ())
}

fn object_format(oid: &str) -> Result<GitObjectFormat, QueryError> {
    let format = match oid.len() {
        40 => GitObjectFormat::Sha1,
        64 => GitObjectFormat::Sha256,
        _ => {
            return Err(QueryError::Backend(
                "commit lineage target is not a full object ID".to_owned(),
            ));
        }
    };
    if !oid
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(QueryError::Backend(
            "commit lineage target is not a canonical object ID".to_owned(),
        ));
    }
    Ok(format)
}

fn exact_commit(
    resource: &Resource,
    repository: &Resource,
    format: GitObjectFormat,
) -> Result<ExactCommit, QueryError> {
    if resource.kind != ResourceKind::Commit
        || resource.logical_repository.as_ref() != Some(&repository.id)
        || object_format(&resource.display)? != format
    {
        return Err(QueryError::Backend(
            "commit operation crossed an exact repository or object-format boundary".to_owned(),
        ));
    }
    Ok(ExactCommit {
        resource: resource.clone(),
        repository: repository.clone(),
        object_format: format,
        oid: resource.display.clone(),
    })
}

fn metadata_matches_shape(metadata: &OperationMetadata) -> bool {
    let expected = match metadata.kind {
        CommitLineageOperationKind::Amend | CommitLineageOperationKind::Rebase => {
            CommitLineageRelationClass::Replacement
        }
        CommitLineageOperationKind::CherryPick => CommitLineageRelationClass::Derivation,
    };
    metadata.relation_class == expected
        && metadata.proof_class == CommitLineageProofClass::RepositoryVerified
        && metadata.state == CommitLineageState::Asserted
        && !metadata.operation_id.is_empty()
        && !metadata.receipt_id.is_empty()
}

fn fact_touches(fact: &OperationFact, current: &Resource) -> bool {
    match &fact.kind {
        OperationFactKind::Mapping { source, result } => {
            source.id == current.id || result.id == current.id
        }
        OperationFactKind::Yield { result, .. } => result.id == current.id,
    }
}

fn fact_in_repository(fact: &OperationFact, repository: &Resource) -> bool {
    let commit_matches = |resource: &Resource| {
        resource.kind == ResourceKind::Commit
            && resource.logical_repository.as_ref() == Some(&repository.id)
            && object_format(&resource.display).ok() == Some(fact.metadata.object_format)
    };
    match &fact.kind {
        OperationFactKind::Mapping { source, result } => {
            commit_matches(source) && commit_matches(result) && source.id != result.id
        }
        OperationFactKind::Yield { result, actor, .. } => {
            commit_matches(result) && actor.kind == ResourceKind::Session
        }
    }
}

fn one_exact_yield(yields: Option<&Vec<YieldFact>>) -> Option<&YieldFact> {
    let yields = yields?;
    match yields.as_slice() {
        [yielded] => Some(yielded),
        _ => None,
    }
}

fn merged_citations(left: &[Citation], right: &[Citation]) -> Vec<Citation> {
    let mut merged = left.to_vec();
    for citation in right {
        if !merged.contains(citation) {
            merged.push(citation.clone());
        }
    }
    merged.sort_by(|left, right| {
        let left = left.evidence();
        let right = right.evidence();
        (
            left.event_sequence,
            left.event_id.to_string(),
            left.session_id.to_string(),
        )
            .cmp(&(
                right.event_sequence,
                right.event_id.to_string(),
                right.session_id.to_string(),
            ))
            .then_with(|| {
                left.source
                    .exact_descriptor_digest()
                    .cmp(&right.source.exact_descriptor_digest())
            })
            .then_with(
                || match (left.byte_range.as_ref(), right.byte_range.as_ref()) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    (Some(left), Some(right)) => {
                        (left.start, left.end_exclusive).cmp(&(right.start, right.end_exclusive))
                    }
                },
            )
            .then_with(|| left.evidence_sha256.cmp(&right.evidence_sha256))
    });
    merged.truncate(crate::protocol::MAX_CITATIONS_PER_FACT);
    merged
}

enum UniqueOrigin {
    Absent,
    Reachable(ExactCommit),
    Unreachable,
    Ambiguous,
}

fn unique_origin(requested: &ExactCommit, edges: &[LineageEdge]) -> UniqueOrigin {
    if edges.is_empty() {
        return UniqueOrigin::Absent;
    }
    if has_directed_cycle(edges) {
        return UniqueOrigin::Ambiguous;
    }
    let mut commits = BTreeMap::from([(requested.resource.id.clone(), requested.clone())]);
    let mut incoming = BTreeSet::new();
    for edge in edges {
        commits.insert(edge.source.resource.id.clone(), edge.source.clone());
        commits.insert(edge.result.resource.id.clone(), edge.result.clone());
        incoming.insert(edge.result.resource.id.clone());
    }
    let mut roots = commits
        .into_iter()
        .filter(|(id, _)| !incoming.contains(id))
        .map(|(_, commit)| commit);
    let Some(root) = roots.next() else {
        return UniqueOrigin::Ambiguous;
    };
    if roots.next().is_some() {
        return UniqueOrigin::Ambiguous;
    }
    if directed_reachable(&root.resource.id, &requested.resource.id, edges) {
        UniqueOrigin::Reachable(root)
    } else {
        UniqueOrigin::Unreachable
    }
}

fn has_directed_cycle(edges: &[LineageEdge]) -> bool {
    let mut adjacency = BTreeMap::<ResourceId, BTreeSet<ResourceId>>::new();
    let mut indegree = BTreeMap::<ResourceId, usize>::new();
    for edge in edges {
        indegree.entry(edge.source.resource.id.clone()).or_default();
        indegree.entry(edge.result.resource.id.clone()).or_default();
        if adjacency
            .entry(edge.source.resource.id.clone())
            .or_default()
            .insert(edge.result.resource.id.clone())
        {
            *indegree.entry(edge.result.resource.id.clone()).or_default() += 1;
        }
    }
    let mut pending = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(id.clone()))
        .collect::<VecDeque<_>>();
    let mut visited = 0_usize;
    while let Some(current) = pending.pop_front() {
        visited = visited.saturating_add(1);
        for next in adjacency.get(&current).into_iter().flatten() {
            let Some(degree) = indegree.get_mut(next) else {
                return true;
            };
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                pending.push_back(next.clone());
            }
        }
    }
    visited != indegree.len()
}

fn directed_reachable(start: &ResourceId, target: &ResourceId, edges: &[LineageEdge]) -> bool {
    let mut pending = VecDeque::from([start.clone()]);
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if current == *target {
            return true;
        }
        for edge in edges
            .iter()
            .filter(|edge| edge.source.resource.id == current)
        {
            if !visited.contains(&edge.result.resource.id) {
                pending.push_back(edge.result.resource.id.clone());
            }
        }
    }
    false
}

fn operation_fact_order(left: &OperationFact, right: &OperationFact) -> std::cmp::Ordering {
    fn key(fact: &OperationFact) -> (&str, u8, &str, &str) {
        match &fact.kind {
            OperationFactKind::Mapping { source, result } => (
                fact.metadata.operation_id.as_str(),
                0,
                source.display.as_str(),
                result.display.as_str(),
            ),
            OperationFactKind::Yield { result, actor, .. } => (
                fact.metadata.operation_id.as_str(),
                1,
                result.display.as_str(),
                actor.id.0.as_str(),
            ),
        }
    }
    key(left).cmp(&key(right))
}

#[cfg(test)]
mod tests;

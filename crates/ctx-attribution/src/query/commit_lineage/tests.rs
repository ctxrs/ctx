//! Exact commit-lineage graph tests.

use super::*;

const REPOSITORY_ID: &str = "repository:fixture";

#[derive(Default)]
struct FakeGraph {
    by_commit: BTreeMap<ResourceId, BTreeSet<String>>,
    by_operation: BTreeMap<String, Vec<OperationFact>>,
}

impl FakeGraph {
    fn add(&mut self, fact: OperationFact) {
        let operation_id = fact.metadata.operation_id.clone();
        match &fact.kind {
            OperationFactKind::Mapping { source, result } => {
                self.by_commit
                    .entry(source.id.clone())
                    .or_default()
                    .insert(operation_id.clone());
                self.by_commit
                    .entry(result.id.clone())
                    .or_default()
                    .insert(operation_id.clone());
            }
            OperationFactKind::Yield { result, .. } => {
                self.by_commit
                    .entry(result.id.clone())
                    .or_default()
                    .insert(operation_id.clone());
            }
        }
        self.by_operation
            .entry(operation_id)
            .or_default()
            .push(fact);
    }
}

impl CommitLineageGraph for FakeGraph {
    fn operation_ids_for_commit(
        &self,
        commit: &Resource,
        repository: &Resource,
        excluded: &BTreeSet<String>,
        limit: usize,
    ) -> Result<OperationIdPage, QueryError> {
        assert_eq!(repository.id.0, REPOSITORY_ID);
        let operation_ids = self
            .by_commit
            .get(&commit.id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|operation_id| !excluded.contains(operation_id))
            .collect::<BTreeSet<_>>();
        let has_more = operation_ids.len() > limit;
        Ok(OperationIdPage {
            operation_ids: operation_ids.into_iter().take(limit).collect(),
            has_more,
        })
    }

    fn operation_facts_for_id(
        &self,
        operation_id: &str,
        repository: &Resource,
        limit: usize,
    ) -> Result<OperationGroupPage, QueryError> {
        assert_eq!(repository.id.0, REPOSITORY_ID);
        let facts = self
            .by_operation
            .get(operation_id)
            .cloned()
            .unwrap_or_default();
        let has_more = facts.len() > limit;
        Ok(OperationGroupPage {
            facts: facts.into_iter().take(limit).collect(),
            has_more,
        })
    }
}

fn repository() -> Resource {
    Resource {
        id: ResourceId(REPOSITORY_ID.to_owned()),
        kind: ResourceKind::Repository,
        display: "forge:example.test/acme/repo".to_owned(),
        logical_repository: None,
    }
}

fn commit(digit: char) -> Resource {
    let oid = digit.to_string().repeat(40);
    commit_oid(oid)
}

fn commit_number(number: usize) -> Resource {
    commit_oid(format!("{number:040x}"))
}

fn commit_oid(oid: String) -> Resource {
    Resource {
        id: ResourceId(format!("commit:{oid}")),
        kind: ResourceKind::Commit,
        display: oid,
        logical_repository: Some(ResourceId(REPOSITORY_ID.to_owned())),
    }
}

fn session(name: &str) -> Resource {
    Resource {
        id: ResourceId(format!("session:{name}")),
        kind: ResourceKind::Session,
        display: name.to_owned(),
        logical_repository: None,
    }
}

fn metadata(id: &str) -> OperationMetadata {
    OperationMetadata {
        operation_id: format!("operation:{id}"),
        receipt_id: format!("receipt:{id}"),
        kind: CommitLineageOperationKind::Rebase,
        relation_class: CommitLineageRelationClass::Replacement,
        proof_class: CommitLineageProofClass::RepositoryVerified,
        state: CommitLineageState::Asserted,
        object_format: GitObjectFormat::Sha1,
        observed_at_ms: Some(10),
        evidence_identity: OperationEvidenceIdentity {
            source_id: "source:fixture".to_owned(),
            event_id: format!("event:{id}"),
            direct_session_id: format!("session:{id}"),
            root_session_id: Some("run:fixture".to_owned()),
            event_sequence: 10,
        },
    }
}

fn operation(id: &str, source: Resource, result: Resource) -> [OperationFact; 2] {
    let metadata = metadata(id);
    [
        OperationFact {
            metadata: metadata.clone(),
            kind: OperationFactKind::Mapping {
                source,
                result: result.clone(),
            },
            citations: Vec::new(),
        },
        OperationFact {
            metadata,
            kind: OperationFactKind::Yield {
                yield_id: format!("yield:{id}"),
                result,
                actor: session(id),
            },
            citations: Vec::new(),
        },
    ]
}

fn add_operation(graph: &mut FakeGraph, id: &str, source: Resource, result: Resource) {
    for fact in operation(id, source, result) {
        graph.add(fact);
    }
}

fn add_mapping_to_operation(graph: &mut FakeGraph, id: &str, source: Resource, result: Resource) {
    add_operation(graph, id, source, result);
}

fn add_mapping_without_yield(graph: &mut FakeGraph, id: &str, source: Resource, result: Resource) {
    graph.add(OperationFact {
        metadata: metadata(id),
        kind: OperationFactKind::Mapping { source, result },
        citations: Vec::new(),
    });
}

fn add_yield(graph: &mut FakeGraph, id: &str, result: Resource, actor: Resource) {
    graph.add(OperationFact {
        metadata: metadata(id),
        kind: OperationFactKind::Yield {
            yield_id: format!("yield:{id}"),
            result,
            actor,
        },
        citations: Vec::new(),
    });
}

#[test]
fn chain_keeps_the_requested_old_sha_and_finds_one_origin() {
    let [old, middle, newest] = [commit('1'), commit('2'), commit('3')];
    let mut graph = FakeGraph::default();
    add_operation(&mut graph, "first", old.clone(), middle.clone());
    add_operation(&mut graph, "second", middle, newest);

    let lineage = compose(&graph, old.clone(), repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.requested.resource, old);
    assert_eq!(lineage.edges.len(), 2);
    assert_eq!(
        lineage.origin.as_ref().map(|origin| &origin.resource),
        Some(&old)
    );
    assert_eq!(lineage.returned_operations, 2);
    assert!(lineage.complete);
    assert!(!lineage.ambiguous);
}

#[test]
fn branch_is_complete_and_deterministic_without_actor_transfer() {
    let source = commit('1');
    let [left, right] = [commit('2'), commit('3')];
    let mut graph = FakeGraph::default();
    add_operation(&mut graph, "left", source.clone(), left.clone());
    add_operation(&mut graph, "right", source.clone(), right.clone());

    let lineage = compose(&graph, left, repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.edges.len(), 2);
    assert_eq!(
        lineage.origin.as_ref().map(|origin| &origin.resource),
        Some(&source)
    );
    assert_eq!(lineage.edges[0].actor.id.0, "session:left");
    assert_eq!(lineage.edges[1].actor.id.0, "session:right");
}

#[test]
fn cycle_terminates_and_abstains_from_a_unique_origin() {
    let [first, second, third] = [commit('1'), commit('2'), commit('3')];
    let mut graph = FakeGraph::default();
    add_operation(&mut graph, "one", first.clone(), second.clone());
    add_operation(&mut graph, "two", second, third.clone());
    add_operation(&mut graph, "three", third, first.clone());

    let lineage = compose(&graph, first.clone(), repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.requested.resource, first);
    assert_eq!(lineage.edges.len(), 3);
    assert_eq!(lineage.origin, None);
    assert!(lineage.complete);
    assert!(lineage.ambiguous);
}

#[test]
fn rooted_graph_with_a_downstream_cycle_is_ambiguous() {
    let [root, first, requested] = [commit('1'), commit('2'), commit('3')];
    let mut graph = FakeGraph::default();
    add_operation(&mut graph, "root", root, first.clone());
    add_operation(&mut graph, "forward", first.clone(), requested.clone());
    add_operation(&mut graph, "cycle", requested.clone(), first);

    let lineage = compose(&graph, requested, repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.origin, None);
    assert!(lineage.complete);
    assert!(lineage.ambiguous);
}

#[test]
fn multiple_directed_roots_are_complete_but_ambiguous() {
    let [left, right, requested] = [commit('1'), commit('2'), commit('3')];
    let mut graph = FakeGraph::default();
    add_operation(&mut graph, "left", left, requested.clone());
    add_operation(&mut graph, "right", right, requested.clone());

    let lineage = compose(&graph, requested, repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.edges.len(), 2);
    assert_eq!(lineage.origin, None);
    assert!(lineage.complete);
    assert!(lineage.ambiguous);
}

#[test]
fn missing_operation_yield_is_a_truthful_evidence_gap() {
    let requested = commit('1');
    let mut graph = FakeGraph::default();
    add_mapping_without_yield(&mut graph, "gap", requested.clone(), commit('2'));
    add_mapping_without_yield(&mut graph, "gap", commit('3'), commit('4'));

    let lineage = compose(&graph, requested, repository())
        .unwrap()
        .expect("partial lineage");
    assert!(lineage.edges.is_empty());
    assert!(lineage.yielded_by.is_empty());
    assert_eq!(lineage.origin, None);
    assert_eq!(lineage.returned_operations, 0);
    assert_eq!(lineage.examined_operations, 1);
    // Omission is counted in operation events, not plural mappings.
    assert_eq!(lineage.omitted_at_least, 1);
    assert_eq!(lineage.truncation, Some(LineageTruncation::EvidenceGap));
    assert!(!lineage.complete);
    assert!(lineage.ambiguous);
}

#[test]
fn unique_indegree_zero_commit_that_cannot_reach_request_is_not_an_origin() {
    let [root, bridge, requested, cycle_peer] =
        [commit('1'), commit('2'), commit('3'), commit('4')];
    let mut graph = FakeGraph::default();
    add_operation(&mut graph, "root-to-bridge", root, bridge.clone());
    add_operation(&mut graph, "request-to-bridge", requested.clone(), bridge);
    add_operation(
        &mut graph,
        "request-to-cycle",
        requested.clone(),
        cycle_peer.clone(),
    );
    add_operation(
        &mut graph,
        "cycle-to-request",
        cycle_peer,
        requested.clone(),
    );

    let lineage = compose(&graph, requested.clone(), repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.requested.resource, requested);
    assert_eq!(lineage.edges.len(), 4);
    assert_eq!(lineage.origin, None);
    assert!(lineage.complete);
    assert!(lineage.ambiguous);
}

#[test]
fn exact_operation_lookup_keeps_a_large_plural_group_with_disconnected_pairs() {
    const MAPPINGS: usize = 32;
    let mut graph = FakeGraph::default();
    for index in 0..MAPPINGS {
        let source = commit_number(index * 2 + 1);
        let result = commit_number(index * 2 + 2);
        add_mapping_to_operation(&mut graph, "plural", source, result);
    }
    let requested = commit_number(1);

    let lineage = compose(&graph, requested.clone(), repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.requested.resource, requested);
    assert_eq!(lineage.edges.len(), MAPPINGS);
    assert_eq!(lineage.returned_operations, 1);
    assert_eq!(lineage.examined_operations, 1);
    assert!(lineage.complete);
    assert!(
        lineage
            .edges
            .iter()
            .any(|edge| edge.source.resource == commit_number(63)
                && edge.result.resource == commit_number(64))
    );
}

#[test]
fn operation_group_above_32_mappings_is_rejected() {
    let mut graph = FakeGraph::default();
    for index in 0..33 {
        add_mapping_to_operation(
            &mut graph,
            "over-bound",
            commit_number(index * 2 + 1),
            commit_number(index * 2 + 2),
        );
    }

    assert!(matches!(
        compose(&graph, commit_number(1), repository()),
        Err(QueryError::Backend(message))
            if message == "commit operation group exceeds its query fact bound"
    ));
}

#[test]
fn multiple_standalone_yields_keep_each_actor_on_its_exact_event() {
    let requested = commit('1');
    let mut graph = FakeGraph::default();
    add_yield(&mut graph, "alpha", requested.clone(), session("alpha"));
    add_yield(&mut graph, "beta", requested.clone(), session("beta"));

    let lineage = compose(&graph, requested, repository())
        .unwrap()
        .expect("lineage");
    assert!(lineage.edges.is_empty());
    assert_eq!(lineage.yielded_by.len(), 2);
    assert_eq!(lineage.returned_operations, 2);
    assert_eq!(lineage.yielded_by[0].actor.id.0, "session:alpha");
    assert_eq!(lineage.yielded_by[1].actor.id.0, "session:beta");
}

#[test]
fn pull_request_only_activity_cannot_become_commit_operation_lineage() {
    // PR facts are outside `CommitLineageGraph`; an exact commit with only
    // PR activity therefore causes an explicit lineage abstention.
    assert_eq!(
        compose(&FakeGraph::default(), commit('1'), repository()).unwrap(),
        None
    );
}

#[test]
fn returned_limit_counts_distinct_operations_not_facts() {
    let requested = commit_number(1);
    let mut graph = FakeGraph::default();
    for index in 0..=MAX_RETURNED_OPERATIONS {
        add_operation(
            &mut graph,
            &format!("{index:064x}"),
            requested.clone(),
            commit_number(index + 2),
        );
    }

    let lineage = compose(&graph, requested, repository())
        .unwrap()
        .expect("lineage");
    assert_eq!(lineage.edges.len(), MAX_RETURNED_OPERATIONS);
    assert_eq!(lineage.returned_operations, MAX_RETURNED_OPERATIONS);
    assert_eq!(lineage.examined_operations, MAX_RETURNED_OPERATIONS + 1);
    assert_eq!(
        lineage.truncation,
        Some(LineageTruncation::ReturnedOperationLimit)
    );
    assert_eq!(lineage.omitted_at_least, 1);
    assert!(!lineage.complete);
    assert_eq!(lineage.origin, None);
}

#[test]
fn examined_limit_counts_unreturnable_operations_without_false_returned_truncation() {
    let requested = commit_number(1);
    let mut graph = FakeGraph::default();
    for index in 0..=MAX_EXAMINED_OPERATIONS {
        add_mapping_without_yield(
            &mut graph,
            &format!("{index:064x}"),
            requested.clone(),
            commit_number(index + 2),
        );
    }

    let lineage = compose(&graph, requested, repository())
        .unwrap()
        .expect("ambiguous bounded lineage");
    assert!(lineage.edges.is_empty());
    assert_eq!(lineage.returned_operations, 0);
    assert_eq!(lineage.examined_operations, MAX_EXAMINED_OPERATIONS);
    assert_eq!(
        lineage.truncation,
        Some(LineageTruncation::ExaminedOperationLimit)
    );
    assert!(lineage.ambiguous);
    assert!(!lineage.complete);
    assert_eq!(lineage.origin, None);
}

use super::*;

fn commit(id: &str, digit: char) -> ExactCommitRef {
    let oid = digit.to_string().repeat(40);
    ExactCommitRef {
        resource: ResourceRef {
            id: format!("commit:{id}"),
            kind: ResourceKind::Commit,
            display: oid.clone(),
        },
        logical_repository_id: "forge:github.com/ctxrs/ctx".to_owned(),
        object_format: GitObjectFormat::Sha1,
        oid,
    }
}

fn session(id: &str) -> ResourceRef {
    ResourceRef {
        id: format!("session:{id}"),
        kind: ResourceKind::Session,
        display: id.to_owned(),
    }
}

fn numbered_commit(number: usize) -> ExactCommitRef {
    let oid = format!("{number:040x}");
    ExactCommitRef {
        resource: ResourceRef {
            id: format!("commit:{oid}"),
            kind: ResourceKind::Commit,
            display: oid.clone(),
        },
        logical_repository_id: "forge:github.com/ctxrs/ctx".to_owned(),
        object_format: GitObjectFormat::Sha1,
        oid,
    }
}

fn yielded(operation_id: &str) -> CommitLineageYield {
    CommitLineageYield {
        yield_id: format!("yield:{operation_id}"),
        operation_id: canonical_operation_id(operation_id),
        logical_repository_id: "forge:github.com/ctxrs/ctx".to_owned(),
        actor: session("operator"),
        proof_class: CommitLineageProofClass::RepositoryVerified,
        state: CommitLineageState::Asserted,
        observed_at_ms: Some(1_700_000_000_000),
        evidence_numbers: vec![1],
    }
}

fn edge(
    operation_id: &str,
    kind: CommitLineageOperationKind,
    source: ExactCommitRef,
    result: ExactCommitRef,
) -> CommitLineageEdge {
    CommitLineageEdge {
        operation_id: canonical_operation_id(operation_id),
        kind,
        relation_class: if kind == CommitLineageOperationKind::CherryPick {
            CommitLineageRelationClass::Derivation
        } else {
            CommitLineageRelationClass::Replacement
        },
        source,
        result,
        actor: session("operator"),
        proof_class: CommitLineageProofClass::RepositoryVerified,
        state: CommitLineageState::Asserted,
        observed_at_ms: Some(1_700_000_000_000),
        evidence_numbers: vec![1],
    }
}

fn canonical_operation_id(label: &str) -> String {
    let mut digest = String::with_capacity(64);
    for byte in label.bytes().take(32) {
        digest.push_str(&format!("{byte:02x}"));
    }
    while digest.len() < 64 {
        digest.push('0');
    }
    digest
}

fn complete_lineage() -> CommitLineage {
    let source = commit("source", '1');
    let requested = commit("requested", '2');
    CommitLineage {
        requested: requested.clone(),
        edges: vec![edge(
            "operation:rebase",
            CommitLineageOperationKind::Rebase,
            source.clone(),
            requested.clone(),
        )],
        yielded_by: Vec::new(),
        origin: Some(source),
        endpoint: Some(ScopedCommitEndpoint::CurrentAtRef {
            commit: requested,
            scope: ResourceRef {
                id: "branch:main".to_owned(),
                kind: ResourceKind::Branch,
                display: "main".to_owned(),
            },
            observation_id: "observation:main".to_owned(),
            observed_at_ms: 1_700_000_000_000,
            evidence_numbers: vec![1],
        }),
        complete: true,
        ambiguous: false,
        bounds: CommitLineageBounds {
            returned_events: 1,
            returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
            examined_events: 1,
            examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
            omission: CommitLineageOmission::Exact(0),
            truncation_reason: None,
        },
    }
}

fn validate(lineage: &CommitLineage) -> Result<BTreeSet<u32>, ProtocolError> {
    let available = BTreeSet::from([1]);
    let mut referenced = BTreeSet::new();
    lineage.validate(
        &lineage.requested.resource,
        &ResourceRef {
            id: "repository:ctxrs-ctx".to_owned(),
            kind: ResourceKind::Repository,
            display: lineage.requested.logical_repository_id.clone(),
        },
        &available,
        &mut referenced,
    )?;
    Ok(referenced)
}

#[test]
fn complete_exact_lineage_preserves_requested_object_and_all_references() {
    let lineage = complete_lineage();
    assert_eq!(validate(&lineage).unwrap(), BTreeSet::from([1]));
    let encoded = serde_json::to_value(&lineage).unwrap();
    assert_eq!(encoded["requested"]["oid"], "2".repeat(40));
    assert_eq!(encoded["edges"][0]["kind"], "rebase");
    assert_eq!(encoded["endpoint"]["kind"], "current_at_ref");
    assert!(encoded.get("current").is_none());
}

#[test]
fn amend_and_cherry_pick_have_closed_distinct_relation_classes() {
    let source = commit("source", '1');
    let result = commit("result", '2');
    for (kind, expected) in [
        (
            CommitLineageOperationKind::Amend,
            CommitLineageRelationClass::Replacement,
        ),
        (
            CommitLineageOperationKind::Rebase,
            CommitLineageRelationClass::Replacement,
        ),
        (
            CommitLineageOperationKind::CherryPick,
            CommitLineageRelationClass::Derivation,
        ),
    ] {
        let mut value = edge("operation:test", kind, source.clone(), result.clone());
        value.relation_class = expected;
        let available = BTreeSet::from([1]);
        value.validate(&available, &mut BTreeSet::new()).unwrap();
        value.relation_class = if expected == CommitLineageRelationClass::Replacement {
            CommitLineageRelationClass::Derivation
        } else {
            CommitLineageRelationClass::Replacement
        };
        assert!(value.validate(&available, &mut BTreeSet::new()).is_err());
    }
}

#[test]
fn exact_commit_rejects_abbreviated_mismatched_and_uppercase_oids() {
    let mut value = commit("commit", 'a');
    value.oid.pop();
    assert!(value.validate().is_err());

    let mut value = commit("commit", 'a');
    value.oid = "A".repeat(40);
    value.resource.display = value.oid.clone();
    assert!(value.validate().is_err());

    let mut value = commit("commit", 'a');
    value.object_format = GitObjectFormat::Sha256;
    assert!(value.validate().is_err());
}

#[test]
fn operation_ids_require_canonical_lowercase_sha256() {
    for invalid in [
        "a".repeat(63),
        "A".repeat(64),
        format!("{}\n", "a".repeat(63)),
    ] {
        let mut lineage = complete_lineage();
        lineage.edges[0].operation_id = invalid.clone();
        assert!(validate(&lineage).is_err());

        let mut lineage = complete_lineage();
        lineage.edges.clear();
        lineage.origin = Some(lineage.requested.clone());
        lineage.yielded_by = vec![yielded("operation:yield")];
        lineage.yielded_by[0].operation_id = invalid;
        assert!(validate(&lineage).is_err());
    }
}

#[test]
fn lineage_rejects_mismatched_requested_oid_and_repository_domain() {
    let lineage = complete_lineage();
    let available = BTreeSet::from([1]);
    let repository = ResourceRef {
        id: "repository:ctxrs-ctx".to_owned(),
        kind: ResourceKind::Repository,
        display: lineage.requested.logical_repository_id.clone(),
    };

    let mut mismatched_target = lineage.requested.resource.clone();
    mismatched_target.display = "f".repeat(40);
    assert!(lineage
        .validate(
            &mismatched_target,
            &repository,
            &available,
            &mut BTreeSet::new(),
        )
        .is_err());

    let mut cross_repository = lineage.clone();
    cross_repository.edges[0].source.logical_repository_id =
        "forge:github.com/ctxrs/other".to_owned();
    assert!(validate(&cross_repository).is_err());

    let mut cross_repository_yield = lineage.clone();
    cross_repository_yield.edges.clear();
    cross_repository_yield.origin = Some(cross_repository_yield.requested.clone());
    cross_repository_yield.yielded_by = vec![yielded("operation:yield")];
    cross_repository_yield.yielded_by[0].logical_repository_id =
        "forge:github.com/ctxrs/other".to_owned();
    assert!(validate(&cross_repository_yield).is_err());

    let mut wrong_repository = repository;
    wrong_repository.display = "forge:github.com/ctxrs/other".to_owned();
    assert!(lineage
        .validate(
            &lineage.requested.resource,
            &wrong_repository,
            &available,
            &mut BTreeSet::new(),
        )
        .is_err());
}

#[test]
fn asserted_operation_edges_and_yields_require_repository_verified_proof() {
    for proof_class in [
        CommitLineageProofClass::RecordExact,
        CommitLineageProofClass::ForgeVerified,
    ] {
        let mut lineage = complete_lineage();
        lineage.edges[0].proof_class = proof_class;
        assert!(validate(&lineage).is_err());

        let mut lineage = complete_lineage();
        lineage.edges.clear();
        lineage.origin = Some(lineage.requested.clone());
        lineage.yielded_by = vec![yielded("operation:yield")];
        lineage.yielded_by[0].proof_class = proof_class;
        assert!(validate(&lineage).is_err());
    }

    let mut ambiguous = complete_lineage();
    ambiguous.edges[0].proof_class = CommitLineageProofClass::RecordExact;
    ambiguous.edges[0].state = CommitLineageState::Ambiguous;
    ambiguous.ambiguous = true;
    ambiguous.origin = None;
    ambiguous.endpoint = None;
    validate(&ambiguous).unwrap();
}

#[test]
fn plural_mappings_group_by_operation_and_require_consistent_metadata() {
    let mut lineage = complete_lineage();
    lineage.origin = None;
    lineage.endpoint = None;
    lineage.edges.push(edge(
        "operation:rebase",
        CommitLineageOperationKind::Rebase,
        commit("side-source", '3'),
        commit("side-result", '4'),
    ));
    lineage.edges.sort_by(CommitLineageEdge::stable_cmp);
    validate(&lineage).unwrap();
    assert_eq!(lineage.bounds.returned_events, 1);

    let mut inconsistent = lineage.clone();
    inconsistent.edges[1].actor = session("different-operator");
    assert!(validate(&inconsistent).is_err());

    let mut inconsistent = lineage.clone();
    inconsistent.edges[1].observed_at_ms = Some(1_700_000_000_001);
    assert!(validate(&inconsistent).is_err());

    let mut inconsistent = lineage.clone();
    inconsistent.edges[1].evidence_numbers.clear();
    assert!(validate(&inconsistent).is_err());

    let mut inconsistent = lineage.clone();
    for edge in &mut inconsistent.edges {
        edge.state = CommitLineageState::Ambiguous;
        edge.proof_class = CommitLineageProofClass::RecordExact;
    }
    inconsistent.ambiguous = true;
    inconsistent.edges[1].proof_class = CommitLineageProofClass::ForgeVerified;
    assert!(validate(&inconsistent).is_err());

    let mut inconsistent = lineage.clone();
    inconsistent.ambiguous = true;
    inconsistent.edges[1].state = CommitLineageState::Ambiguous;
    assert!(validate(&inconsistent).is_err());

    let mut inconsistent = lineage;
    inconsistent.edges[1].kind = CommitLineageOperationKind::CherryPick;
    inconsistent.edges[1].relation_class = CommitLineageRelationClass::Derivation;
    assert!(validate(&inconsistent).is_err());
}

#[test]
fn edge_and_yield_with_one_operation_id_count_once_and_share_metadata() {
    let requested = commit("requested", '2');
    let mut lineage = CommitLineage {
        requested: requested.clone(),
        edges: vec![edge(
            "operation:mixed",
            CommitLineageOperationKind::CherryPick,
            requested.clone(),
            commit("descendant", '3'),
        )],
        yielded_by: vec![yielded("operation:mixed")],
        origin: None,
        endpoint: None,
        complete: true,
        ambiguous: false,
        bounds: CommitLineageBounds {
            returned_events: 1,
            returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
            examined_events: 1,
            examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
            omission: CommitLineageOmission::Exact(0),
            truncation_reason: None,
        },
    };
    validate(&lineage).unwrap();

    lineage.yielded_by[0].actor = session("different-operator");
    assert!(validate(&lineage).is_err());
}

#[test]
fn returned_event_bound_counts_distinct_operations_at_99_100_and_101() {
    for count in [99_usize, 100] {
        let lineage = distinct_operation_chain(count);
        validate(&lineage).unwrap();
    }
    assert!(validate(&distinct_operation_chain(101)).is_err());
}

#[test]
fn per_operation_mapping_bound_accepts_exactly_32_edges_plus_yields() {
    for lineage in [
        single_operation_group(MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS, false),
        single_operation_group(MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS - 1, true),
    ] {
        validate(&lineage).unwrap();
        assert_eq!(
            lineage.edges.len() + lineage.yielded_by.len(),
            MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS
        );
        assert_eq!(lineage.bounds.returned_events, 1);
    }
}

#[test]
fn per_operation_mapping_bound_rejects_33_edges_plus_yields() {
    for lineage in [
        single_operation_group(MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS + 1, false),
        single_operation_group(MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS, true),
    ] {
        assert_eq!(
            lineage.edges.len() + lineage.yielded_by.len(),
            MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS + 1
        );
        assert!(validate(&lineage).is_err());
    }
}

#[test]
fn disconnected_operations_are_rejected() {
    let mut lineage = complete_lineage();
    lineage.origin = None;
    lineage.endpoint = None;
    lineage.edges.push(edge(
        "operation:unrelated",
        CommitLineageOperationKind::Amend,
        commit("unrelated-source", '3'),
        commit("unrelated-result", '4'),
    ));
    lineage.edges.sort_by(CommitLineageEdge::stable_cmp);
    lineage.bounds.returned_events = 2;
    lineage.bounds.examined_events = 2;
    assert!(validate(&lineage).is_err());
}

#[test]
fn origin_and_endpoint_follow_directed_ancestry_not_side_branches() {
    let mut lineage = complete_lineage();
    let origin = lineage.origin.clone().unwrap();
    let requested = lineage.requested.clone();
    let descendant = commit("descendant", '3');
    lineage.edges.push(edge(
        "operation:successor",
        CommitLineageOperationKind::CherryPick,
        requested.clone(),
        descendant.clone(),
    ));
    lineage.edges.sort_by(CommitLineageEdge::stable_cmp);
    lineage.bounds.returned_events = 2;
    lineage.bounds.examined_events = 2;
    lineage.endpoint = Some(endpoint(descendant.clone()));
    validate(&lineage).unwrap();

    lineage.origin = Some(requested.clone());
    assert!(validate(&lineage).is_err(), "requested is not the root");
    lineage.origin = Some(origin.clone());
    lineage.endpoint = Some(endpoint(origin));
    assert!(validate(&lineage).is_err(), "ancestor is not a descendant");

    let mut side_branch = complete_lineage();
    let side = commit("side", '4');
    let root = side_branch.origin.clone().unwrap();
    side_branch.edges.push(edge(
        "operation:side",
        CommitLineageOperationKind::CherryPick,
        root,
        side.clone(),
    ));
    side_branch.edges.sort_by(CommitLineageEdge::stable_cmp);
    side_branch.bounds.returned_events = 2;
    side_branch.bounds.examined_events = 2;
    side_branch.endpoint = Some(endpoint(side));
    assert!(
        validate(&side_branch).is_err(),
        "a sibling side branch is not reachable from the request"
    );
}

#[test]
fn convergent_directed_roots_require_ambiguity_and_suppress_origin() {
    let mut lineage = complete_lineage();
    let first_root = lineage.origin.take().unwrap();
    lineage.endpoint = None;
    lineage.edges.push(edge(
        "operation:convergent",
        CommitLineageOperationKind::CherryPick,
        commit("second-root", '3'),
        lineage.requested.clone(),
    ));
    lineage.edges.sort_by(CommitLineageEdge::stable_cmp);
    lineage.bounds.returned_events = 2;
    lineage.bounds.examined_events = 2;

    let error = validate(&lineage).unwrap_err();
    assert_eq!(
        error.message,
        "complete asserted commit lineage with multiple directed roots must report ambiguity"
    );

    lineage.ambiguous = true;
    validate(&lineage).unwrap();

    lineage.origin = Some(first_root);
    assert!(
        validate(&lineage).is_err(),
        "ambiguous convergent roots cannot claim one root as the origin"
    );
}

#[test]
fn partial_or_ambiguous_lineage_suppresses_origin_and_endpoint() {
    let mut partial = complete_lineage();
    partial.complete = false;
    partial.bounds.returned_events = MAX_COMMIT_LINEAGE_RETURNED_EVENTS;
    partial.bounds.examined_events = MAX_COMMIT_LINEAGE_RETURNED_EVENTS;
    partial.bounds.omission = CommitLineageOmission::AtLeast(1);
    partial.bounds.truncation_reason = Some(CommitLineageTruncationReason::ReturnedEventLimit);
    assert!(validate(&partial).is_err());
    partial.origin = None;
    partial.endpoint = None;
    assert!(
        validate(&partial).is_err(),
        "actual retained count still disagrees"
    );

    partial.bounds.returned_events = 1;
    partial.bounds.returned_event_limit = 1;
    assert!(
        validate(&partial).is_err(),
        "published limit is not deterministic"
    );

    let mut ambiguous = complete_lineage();
    ambiguous.ambiguous = true;
    assert!(validate(&ambiguous).is_err());
    ambiguous.origin = None;
    ambiguous.endpoint = None;
    validate(&ambiguous).unwrap();
}

#[test]
fn incomplete_bounds_require_nonzero_or_unknown_omission_and_reached_limit() {
    let mut lineage = complete_lineage();
    lineage.origin = None;
    lineage.endpoint = None;
    lineage.complete = false;
    lineage.bounds.omission = CommitLineageOmission::Unknown;
    lineage.bounds.truncation_reason = Some(CommitLineageTruncationReason::ExaminedEventLimit);
    assert!(validate(&lineage).is_err());
    lineage.bounds.examined_events = MAX_COMMIT_LINEAGE_EXAMINED_EVENTS;
    validate(&lineage).unwrap();

    lineage.bounds.omission = CommitLineageOmission::AtLeast(0);
    assert!(validate(&lineage).is_err());
    lineage.bounds.omission = CommitLineageOmission::Exact(0);
    assert!(validate(&lineage).is_err());
}

#[test]
fn evidence_gap_partial_bounds_require_truthful_non_limit_state() {
    assert_eq!(
        serde_json::to_value(CommitLineageTruncationReason::EvidenceGap).unwrap(),
        serde_json::json!("evidence_gap")
    );

    let mut lineage = complete_lineage();
    lineage.origin = None;
    lineage.endpoint = None;
    lineage.complete = false;
    lineage.bounds.omission = CommitLineageOmission::Unknown;
    lineage.bounds.truncation_reason = Some(CommitLineageTruncationReason::EvidenceGap);
    validate(&lineage).unwrap();

    let mut complete = lineage.clone();
    complete.complete = true;
    assert!(
        validate(&complete).is_err(),
        "complete lineage cannot report an evidence gap"
    );

    let mut no_omission = lineage.clone();
    no_omission.bounds.omission = CommitLineageOmission::Exact(0);
    assert!(
        validate(&no_omission).is_err(),
        "evidence gaps must report omitted lineage"
    );

    let mut stale_returned_count = lineage.clone();
    stale_returned_count.bounds.returned_events = 2;
    assert!(
        validate(&stale_returned_count).is_err(),
        "evidence gaps cannot weaken exact returned-operation accounting"
    );

    lineage.bounds.examined_events = MAX_COMMIT_LINEAGE_EXAMINED_EVENTS;
    assert!(
        validate(&lineage).is_err(),
        "an exhausted examined-event bound must use its exact limit reason"
    );
}

#[test]
fn incoming_operation_and_standalone_yield_cannot_duplicate_the_actor() {
    let mut lineage = complete_lineage();
    lineage.origin = None;
    lineage.endpoint = None;
    lineage.yielded_by.push(CommitLineageYield {
        yield_id: "yield:duplicate".to_owned(),
        operation_id: canonical_operation_id("operation:rebase"),
        logical_repository_id: lineage.requested.logical_repository_id.clone(),
        actor: session("operator"),
        proof_class: CommitLineageProofClass::RepositoryVerified,
        state: CommitLineageState::Asserted,
        observed_at_ms: None,
        evidence_numbers: vec![1],
    });
    lineage.bounds.returned_events = 2;
    assert!(validate(&lineage).is_err());
}

#[test]
fn edge_and_yield_order_is_deterministic_and_strict() {
    let mut lineage = complete_lineage();
    lineage.origin = None;
    lineage.endpoint = None;
    let source = commit("earlier", '0');
    lineage.edges.push(edge(
        "operation:amend",
        CommitLineageOperationKind::Amend,
        source,
        lineage.edges[0].source.clone(),
    ));
    lineage.bounds.returned_events = 2;
    lineage.bounds.examined_events = 2;
    assert!(validate(&lineage).is_err());
    lineage.edges.sort_by(CommitLineageEdge::stable_cmp);
    validate(&lineage).unwrap();
    lineage.edges.push(lineage.edges[0].clone());
    lineage.bounds.returned_events = 3;
    lineage.bounds.examined_events = 3;
    assert!(validate(&lineage).is_err());
}

#[test]
fn endpoint_requires_exact_scope_observation_time_and_citation() {
    let mut lineage = complete_lineage();
    if let Some(ScopedCommitEndpoint::CurrentAtRef { scope, .. }) = lineage.endpoint.as_mut() {
        scope.kind = ResourceKind::Repository;
    }
    assert!(validate(&lineage).is_err());
    if let Some(ScopedCommitEndpoint::CurrentAtRef {
        scope,
        observed_at_ms,
        ..
    }) = lineage.endpoint.as_mut()
    {
        scope.kind = ResourceKind::Branch;
        *observed_at_ms = -1;
    }
    assert!(validate(&lineage).is_err());
}

fn endpoint(commit: ExactCommitRef) -> ScopedCommitEndpoint {
    ScopedCommitEndpoint::CurrentAtRef {
        commit,
        scope: ResourceRef {
            id: "branch:main".to_owned(),
            kind: ResourceKind::Branch,
            display: "main".to_owned(),
        },
        observation_id: "observation:main".to_owned(),
        observed_at_ms: 1_700_000_000_000,
        evidence_numbers: vec![1],
    }
}

fn distinct_operation_chain(count: usize) -> CommitLineage {
    let requested = numbered_commit(0);
    let mut edges = Vec::with_capacity(count);
    for index in 0..count {
        edges.push(edge(
            &format!("operation:{index:03}"),
            CommitLineageOperationKind::CherryPick,
            numbered_commit(index),
            numbered_commit(index + 1),
        ));
    }
    CommitLineage {
        requested: requested.clone(),
        edges,
        yielded_by: Vec::new(),
        origin: Some(requested),
        endpoint: Some(endpoint(numbered_commit(count))),
        complete: true,
        ambiguous: false,
        bounds: CommitLineageBounds {
            returned_events: u32::try_from(count).unwrap(),
            returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
            examined_events: u32::try_from(count).unwrap(),
            examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
            omission: CommitLineageOmission::Exact(0),
            truncation_reason: None,
        },
    }
}

fn single_operation_group(edge_count: usize, include_yield: bool) -> CommitLineage {
    let requested = numbered_commit(0);
    let operation_id = "operation:bounded-group";
    let edges = (0..edge_count)
        .map(|index| {
            edge(
                operation_id,
                CommitLineageOperationKind::Rebase,
                numbered_commit(index * 2),
                numbered_commit(index * 2 + 1),
            )
        })
        .collect();
    let yielded_by = include_yield
        .then(|| yielded(operation_id))
        .into_iter()
        .collect();

    CommitLineage {
        requested,
        edges,
        yielded_by,
        origin: None,
        endpoint: None,
        complete: true,
        ambiguous: false,
        bounds: CommitLineageBounds {
            returned_events: 1,
            returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
            examined_events: 1,
            examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
            omission: CommitLineageOmission::Exact(0),
            truncation_reason: None,
        },
    }
}

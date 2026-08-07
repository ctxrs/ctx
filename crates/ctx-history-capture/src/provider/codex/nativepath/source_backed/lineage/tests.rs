use super::*;
use crate::provider::codex::nativepath::record::CodexLineageRecordEvidence;

fn source(id: &str, parent: Option<&str>, byte: u8) -> (CodexCatalogSource, SourceKey, String) {
    let source_key = codex_source_key(id).unwrap();
    (
        CodexCatalogSource {
            source_root: "/tmp".to_owned(),
            source_path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            cataloged_at_ms: 0,
            catalog_observation: CodexFileObservation {
                len: u64::from(byte),
                modified_at_ms: i64::from(byte),
                stable_token: Some([byte; 32]),
                change_token: [byte.wrapping_add(1); 32],
            },
            catalog_prefix_sha256: Some([byte; 32]),
            catalog_native_session_id: Some(id.to_owned()),
            catalog_parent_native_session_id: parent.map(str::to_owned),
            catalog_session_relationship: if parent.is_some() {
                SessionRelationshipKind::Forked
            } else {
                SessionRelationshipKind::Root
            },
            catalog_advisory_session_id: None,
            catalog_root_native_session_id: None,
            opened: None,
            authority_root: None,
            authority_relative_path: None,
        },
        source_key,
        id.to_owned(),
    )
}

fn related_source(
    id: &str,
    parent: &str,
    relationship: SessionRelationshipKind,
    advisory: Option<&str>,
    byte: u8,
) -> (CodexCatalogSource, SourceKey, String) {
    let mut plan = source(id, Some(parent), byte);
    plan.0.catalog_session_relationship = relationship;
    plan.0.catalog_advisory_session_id = advisory.map(str::to_owned);
    plan
}

#[test]
fn maximum_depth_chain_normalizes_with_linear_dependency_work() {
    let mut sources = Vec::new();
    for index in 0..MAX_CODEX_LINEAGE_NODES {
        let id = format!("node-{index:04}");
        let parent = (index != 0).then(|| format!("node-{:04}", index - 1));
        sources.push(source(&id, parent.as_deref(), (index % 251) as u8));
    }
    let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&sources).unwrap();
    assert!(normalized.rejections.is_empty());
    assert_eq!(normalized.sources.len(), MAX_CODEX_LINEAGE_NODES);
    assert_eq!(
        normalized
            .sources
            .last()
            .unwrap()
            .0
            .catalog_root_native_session_id
            .as_deref(),
        Some("node-0000")
    );
    assert_eq!(
        normalized.authority.dependency_work_units,
        MAX_CODEX_LINEAGE_NODES
    );
    assert_ne!(normalized.authority.dependency_digest("node-1023"), [0; 32]);
}

#[test]
fn over_depth_and_cycle_components_are_rejected_deterministically() {
    let mut sources = Vec::new();
    for index in 0..=MAX_CODEX_LINEAGE_NODES {
        let id = format!("deep-{index:04}");
        let parent = (index != 0).then(|| format!("deep-{:04}", index - 1));
        sources.push(source(&id, parent.as_deref(), (index % 251) as u8));
    }
    for index in 0..4 {
        let id = format!("cycle-{index:04}");
        let parent = format!("cycle-{:04}", (index + 1) % 4);
        sources.push(source(&id, Some(&parent), (index % 251) as u8));
    }
    let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&sources).unwrap();
    assert!(normalized.sources.is_empty());
    assert_eq!(normalized.rejections.len(), sources.len());
    let expected = normalized
        .rejections
        .iter()
        .map(|rejection| serde_json::to_vec(&rejection.proof).unwrap())
        .collect::<Vec<_>>();
    sources.reverse();
    let reversed = CodexOutcomeLineageAuthorityV0::normalize_sources(&sources).unwrap();
    assert_eq!(
        reversed
            .rejections
            .iter()
            .map(|rejection| serde_json::to_vec(&rejection.proof).unwrap())
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn terminal_lineage_states_bypass_poisoned_fact_lock() {
    let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
    let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
    authority.poison_facts_lock();
    assert_eq!(
        authority.classify("root", "call", "call").unwrap(),
        CodexOutcomeOriginV0::UniqueToSession
    );
    assert!(matches!(
        authority.classify("child", "call", "call"),
        Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    ));
}

#[test]
fn fact_budget_exhaustion_is_conservative_and_nonfatal() {
    let sources = vec![source("root", None, 1)];
    let authority = CodexOutcomeLineageAuthorityV0::from_sources_with_budget(
        &sources,
        Arc::new(CodexLineageFactBudgetV0::with_limits(1, 1)),
    )
    .unwrap();
    let facts = authority.new_fact_set("root").unwrap();
    assert_eq!(
        facts.presence("call", "call"),
        CodexLineageFactPresenceV0::Unproven
    );
    authority.register("root", facts).unwrap();
}

#[test]
fn mixed_valid_and_invalid_components_publish_only_valid_sources() {
    let sources = vec![
        source("valid-root", None, 1),
        source("valid-child", Some("valid-root"), 2),
        source("invalid-child", Some("absent"), 3),
        source("invalid-grandchild", Some("invalid-child"), 4),
    ];
    let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&sources).unwrap();
    assert_eq!(
        normalized
            .sources
            .iter()
            .map(|plan| plan.2.as_str())
            .collect::<Vec<_>>(),
        ["valid-child", "valid-root"]
    );
    assert_eq!(normalized.rejections.len(), 2);
    assert!(normalized.rejections.iter().all(|rejection| matches!(
        rejection.proof.reason,
        CodexLineageRejectionReasonV0::MissingParent { .. }
    )));
}

#[test]
fn nested_typed_lineage_and_ancestor_advisories_share_one_transitive_root() {
    let mut root = source("root", None, 1);
    root.0.source_root = "/configured/automatic".to_owned();
    let mut fork = related_source(
        "fork",
        "root",
        SessionRelationshipKind::Forked,
        Some("root"),
        2,
    );
    fork.0.source_root = "/configured/explicit-a".to_owned();
    let mut delegated = related_source(
        "delegated",
        "fork",
        SessionRelationshipKind::Delegated,
        Some("fork"),
        3,
    );
    delegated.0.source_root = "/configured/explicit-b".to_owned();
    let resumed = related_source(
        "resumed",
        "delegated",
        SessionRelationshipKind::ResumedFrom,
        Some("root"),
        4,
    );
    let workflow = related_source(
        "workflow",
        "resumed",
        SessionRelationshipKind::WorkflowChild,
        Some("delegated"),
        5,
    );
    let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&[
        workflow, root, delegated, resumed, fork,
    ])
    .unwrap();
    assert!(normalized.rejections.is_empty());
    assert_eq!(normalized.sources.len(), 5);
    for (source, _, native_session_id) in &normalized.sources {
        assert_eq!(
            source.catalog_root_native_session_id.as_deref(),
            Some("root"),
            "{native_session_id} did not inherit the canonical root"
        );
    }
    assert_eq!(normalized.authority.depth("root"), 0);
    assert_eq!(normalized.authority.depth("workflow"), 4);
}

#[test]
fn valid_normalization_and_dependency_identity_are_permutation_stable() {
    let sources = vec![
        source("root", None, 1),
        related_source(
            "fork",
            "root",
            SessionRelationshipKind::Forked,
            Some("fork"),
            2,
        ),
        related_source(
            "resumed",
            "fork",
            SessionRelationshipKind::ResumedFrom,
            Some("root"),
            3,
        ),
    ];
    let forward = CodexOutcomeLineageAuthorityV0::normalize_sources(&sources).unwrap();
    let mut reversed_sources = sources;
    reversed_sources.reverse();
    let reversed = CodexOutcomeLineageAuthorityV0::normalize_sources(&reversed_sources).unwrap();
    assert!(forward.rejections.is_empty());
    assert!(reversed.rejections.is_empty());
    assert_eq!(
        forward
            .sources
            .iter()
            .map(|(source, key, native_id)| (
                native_id,
                key,
                source.catalog_parent_native_session_id.as_deref(),
                source.catalog_session_relationship,
                source.catalog_advisory_session_id.as_deref(),
                source.catalog_root_native_session_id.as_deref(),
            ))
            .collect::<Vec<_>>(),
        reversed
            .sources
            .iter()
            .map(|(source, key, native_id)| (
                native_id,
                key,
                source.catalog_parent_native_session_id.as_deref(),
                source.catalog_session_relationship,
                source.catalog_advisory_session_id.as_deref(),
                source.catalog_root_native_session_id.as_deref(),
            ))
            .collect::<Vec<_>>()
    );
    for native_id in ["root", "fork", "resumed"] {
        assert_eq!(
            forward.authority.dependency_digest(native_id),
            reversed.authority.dependency_digest(native_id)
        );
    }
}

#[test]
fn unrelated_advisory_quarantines_only_its_direct_parent_component() {
    let root_a = source("root-a", None, 1);
    let child_a = related_source(
        "child-a",
        "root-a",
        SessionRelationshipKind::Delegated,
        Some("root-b"),
        2,
    );
    let root_b = source("root-b", None, 3);
    let normalized =
        CodexOutcomeLineageAuthorityV0::normalize_sources(&[root_b, child_a, root_a]).unwrap();
    assert_eq!(normalized.sources.len(), 1);
    assert_eq!(normalized.sources[0].2, "root-b");
    assert_eq!(normalized.rejections.len(), 2);
    assert!(normalized.rejections.iter().all(|rejection| matches!(
        rejection.proof.reason,
        CodexLineageRejectionReasonV0::AdvisoryUnrelatedComponent { .. }
    )));
}

#[test]
fn duplicate_self_and_contradictory_components_are_typed_and_all_invalid() {
    let duplicate_left = source("duplicate", None, 1);
    let mut duplicate_right = source("duplicate", None, 2);
    duplicate_right.0.source_path = PathBuf::from("/tmp/duplicate-other.jsonl");
    let duplicate_child = source("duplicate-child", Some("duplicate"), 3);
    let self_parent = source("self", Some("self"), 4);
    let contradictory_parent = source("contradictory-parent", None, 5);
    let mut contradictory = source("contradictory", Some("contradictory-parent"), 6);
    contradictory.0.catalog_session_relationship = SessionRelationshipKind::RelatedUnknown;
    let normalized = CodexOutcomeLineageAuthorityV0::normalize_sources(&[
        contradictory,
        contradictory_parent,
        duplicate_child,
        duplicate_right,
        self_parent,
        duplicate_left,
    ])
    .unwrap();
    assert!(normalized.sources.is_empty());
    assert_eq!(normalized.rejections.len(), 6);
    assert!(normalized.rejections.iter().any(|rejection| matches!(
        rejection.proof.reason,
        CodexLineageRejectionReasonV0::DuplicateNativeSessionId
    )));
    assert!(normalized.rejections.iter().any(|rejection| matches!(
        rejection.proof.reason,
        CodexLineageRejectionReasonV0::SelfParent
    )));
}

#[test]
fn lineage_evidence_authority_requires_certified_absence_for_unique_classification() {
    let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
    let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
    authority
        .register("root", authority.new_fact_set("root").unwrap())
        .unwrap();

    assert_eq!(
        authority.classify("child", "call", "call").unwrap(),
        CodexOutcomeOriginV0::UniqueToSession
    );
}

#[test]
fn carried_parent_participates_and_requires_facts_before_child_classification() {
    let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
    let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
    authority
        .bind_route_sources(&HashSet::from(["child".to_owned()]))
        .unwrap();

    // Exact-route binding includes every transitive carried ancestor in
    // the generation authority. Until that parent's certified or scanned
    // facts arrive, classifying its child is an ordering failure rather
    // than the pre-composition `OutsideRoute`/`Unproven` result.
    assert!(authority.generation_participates("root").unwrap());
    assert!(authority.needs_descendant_facts("root").unwrap());
    assert!(matches!(
        authority.classify("child", "call", "call"),
        Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    ));

    authority
        .register("root", authority.new_fact_set("root").unwrap())
        .unwrap();
    assert_eq!(
        authority.classify("child", "call", "call").unwrap(),
        CodexOutcomeOriginV0::UniqueToSession
    );
}

#[test]
fn selected_parent_without_facts_remains_a_typed_ordering_failure() {
    let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
    let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
    authority
        .bind_route_sources(&HashSet::from(["root".to_owned(), "child".to_owned()]))
        .unwrap();
    assert!(matches!(
        authority.classify("child", "call", "call"),
        Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    ));
}

#[test]
fn registered_terminal_leaf_drops_facts_but_remains_complete() {
    let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
    let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
    authority
        .bind_route_sources(&HashSet::from(["root".to_owned(), "child".to_owned()]))
        .unwrap();

    authority
        .register("child", authority.new_fact_set("child").unwrap())
        .unwrap();
    let facts = authority.facts.lock().unwrap();
    let child = authority.indices["child"];
    assert!(matches!(facts[child], LineageFactsStateV0::CompleteLeaf));
    drop(facts);
    assert!(matches!(
        authority.register("child", authority.new_fact_set("child").unwrap()),
        Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    ));
}

#[test]
fn independent_components_have_stable_partitions_and_release_in_isolation() {
    let sources = vec![
        source("root-a", None, 1),
        source("child-a", Some("root-a"), 2),
        source("root-b", None, 3),
        source("child-b", Some("root-b"), 4),
    ];
    let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
    authority
        .bind_route_sources(&HashSet::from([
            "root-a".to_owned(),
            "child-a".to_owned(),
            "root-b".to_owned(),
            "child-b".to_owned(),
        ]))
        .unwrap();
    let component_a = authority.component_partition("root-a").unwrap();
    let component_b = authority.component_partition("root-b").unwrap();
    assert_eq!(
        component_a,
        authority.component_partition("child-a").unwrap()
    );
    assert_eq!(
        component_b,
        authority.component_partition("child-b").unwrap()
    );
    assert_ne!(component_a, component_b);

    let mut reversed_sources = sources.clone();
    reversed_sources.reverse();
    let reversed = CodexOutcomeLineageAuthorityV0::from_sources(&reversed_sources).unwrap();
    for native_session_id in ["root-a", "child-a", "root-b", "child-b"] {
        assert_eq!(
            authority.component_partition(native_session_id),
            reversed.component_partition(native_session_id)
        );
    }

    for root in ["root-a", "root-b"] {
        let mut facts = authority.new_fact_set(root).unwrap();
        facts
            .record_for_test(CodexLineageRecordEvidence::Call("copied"))
            .unwrap();
        facts
            .record_for_test(CodexLineageRecordEvidence::Result("copied"))
            .unwrap();
        authority.register(root, facts).unwrap();
    }
    authority.release_component(component_a).unwrap();
    assert_eq!(
        authority.classify("child-b", "copied", "copied").unwrap(),
        CodexOutcomeOriginV0::CopiedFromAncestor {
            ancestor_native_session_id: "root-b".to_owned(),
        }
    );
    assert!(matches!(
        authority.classify("child-a", "copied", "copied"),
        Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    ));
}

#[test]
fn component_release_reclaims_its_budget_for_a_conservative_retry() {
    let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
    let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(4096, 2));
    let authority =
        CodexOutcomeLineageAuthorityV0::from_sources_with_budget(&sources, budget).unwrap();
    authority
        .bind_route_sources(&HashSet::from(["root".to_owned(), "child".to_owned()]))
        .unwrap();
    let mut root_facts = authority.new_fact_set("root").unwrap();
    root_facts
        .record_for_test(CodexLineageRecordEvidence::Call("root-call"))
        .unwrap();
    authority.register("root", root_facts).unwrap();
    authority
        .release_component(authority.component_partition("root").unwrap())
        .unwrap();

    let mut retried = authority.new_fact_set("child").unwrap();
    retried
        .record_for_test(CodexLineageRecordEvidence::Call("retry-call"))
        .unwrap();
    assert_eq!(
        retried.presence("retry-call", "missing"),
        CodexLineageFactPresenceV0::Unproven
    );
}

#[test]
fn component_lifetimes_process_more_than_the_old_262144_fact_route_limit() {
    const COMPONENTS: usize = 1_025;
    const FACTS_PER_COMPONENT: usize = 256;
    const OLD_ROUTE_FACT_LIMIT: usize = 262_144;
    const {
        assert!(COMPONENTS * FACTS_PER_COMPONENT > OLD_ROUTE_FACT_LIMIT);
    }

    let mut sources = Vec::with_capacity(COMPONENTS * 2);
    let mut pairs = Vec::with_capacity(COMPONENTS);
    for component in 0..COMPONENTS {
        let root = format!("root-{component:04}");
        let child = format!("child-{component:04}");
        sources.push(source(&root, None, (component % 251) as u8));
        sources.push(source(&child, Some(&root), ((component + 1) % 251) as u8));
        pairs.push((root, child));
    }
    let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
    authority
        .bind_route_sources(
            &pairs
                .iter()
                .flat_map(|(root, child)| [root.clone(), child.clone()])
                .collect(),
        )
        .unwrap();
    pairs.sort_by_key(|(root, _)| authority.component_partition(root).unwrap());

    let mut processed_facts = 0_usize;
    for (component_index, (root, child)) in pairs.iter().enumerate() {
        let marker = format!("copied-{component_index:04}");
        let mut facts = authority.new_fact_set(root).unwrap();
        facts
            .record_for_test(CodexLineageRecordEvidence::Call(&marker))
            .unwrap();
        facts
            .record_for_test(CodexLineageRecordEvidence::Result(&marker))
            .unwrap();
        for fact in 2..FACTS_PER_COMPONENT {
            facts
                .record_for_test(CodexLineageRecordEvidence::Call(&format!(
                    "fact-{component_index:04}-{fact:03}"
                )))
                .unwrap();
        }
        processed_facts += FACTS_PER_COMPONENT;
        authority.register(root, facts).unwrap();
        assert_eq!(
            authority.classify(child, &marker, &marker).unwrap(),
            CodexOutcomeOriginV0::CopiedFromAncestor {
                ancestor_native_session_id: root.clone(),
            }
        );
        authority
            .register(child, authority.new_fact_set(child).unwrap())
            .unwrap();
        let component = authority.component_partition(root).unwrap();
        authority.release_component(component).unwrap();
        let budget = &authority.component_budgets[component as usize];
        assert_eq!(budget.charges_for_test(), (0, 0));
    }
    assert_eq!(processed_facts, COMPONENTS * FACTS_PER_COMPONENT);
    assert!(processed_facts > OLD_ROUTE_FACT_LIMIT);
}

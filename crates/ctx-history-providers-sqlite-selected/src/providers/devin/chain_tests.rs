use std::collections::BTreeMap;

use super::chain::{
    plan_session, DevinLineageKey, DevinNodeFacts, DevinSessionRejection, DevinSpliceKind,
};

/// Builds a forest from `(node_id, parent)` pairs.
fn forest(edges: &[(i64, Option<i64>)]) -> BTreeMap<i64, DevinNodeFacts> {
    edges
        .iter()
        .map(|&(node_id, parent_node_id)| {
            (
                node_id,
                DevinNodeFacts {
                    parent_node_id,
                    ..DevinNodeFacts::default()
                },
            )
        })
        .collect()
}

fn chain_of(facts: &mut BTreeMap<i64, DevinNodeFacts>, nodes: &[i64]) {
    for window in nodes.windows(2) {
        facts.insert(
            window[1],
            DevinNodeFacts {
                parent_node_id: Some(window[0]),
                ..facts.get(&window[1]).cloned().unwrap_or_default()
            },
        );
    }
    facts.entry(nodes[0]).or_default();
}

fn primary_nodes(plan: &super::chain::DevinSessionPlan) -> Vec<i64> {
    plan.lineages[0]
        .nodes
        .iter()
        .map(|node| node.node_id)
        .collect()
}

#[test]
fn a_linear_chain_is_imported_in_ancestor_first_order() {
    let facts = forest(&[(0, None), (1, Some(0)), (2, Some(1)), (3, Some(2))]);
    let plan = plan_session(&facts, Some(3));
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2, 3]);
    assert_eq!(plan.counts.ignored_nodes, 0);
    assert!(plan.lineages[0]
        .nodes
        .iter()
        .all(|node| node.splice_kind == DevinSpliceKind::Chain));
    assert_eq!(
        plan.lineages[0]
            .nodes
            .iter()
            .map(|node| node.chain_ord)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
}

#[test]
fn nodes_off_the_chain_are_counted_and_not_imported() {
    // 4 and 5 are abandoned regenerated turns beside the retained 3.
    let facts = forest(&[
        (0, None),
        (1, Some(0)),
        (2, Some(1)),
        (3, Some(2)),
        (4, Some(2)),
        (5, Some(2)),
        (6, None),
    ]);
    let plan = plan_session(&facts, Some(3));
    assert_eq!(primary_nodes(&plan), [0, 1, 2, 3]);
    assert_eq!(plan.counts.ignored_nodes, 3);
    assert_eq!(plan.imported_nodes(), 4);
}

#[test]
fn a_summarized_from_node_splices_its_prior_history_transitively() {
    // Each compaction starts a fresh chain from its own root, as Devin does,
    // and marks its continuation node with the node it replaced: 9 continues
    // from 6, and 6 continues from 3.
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2, 3]);
    chain_of(&mut facts, &[4, 5, 6]);
    chain_of(&mut facts, &[7, 8, 9]);
    facts.get_mut(&6).unwrap().summarized_from = Some(3);
    facts.get_mut(&9).unwrap().summarized_from = Some(6);

    let plan = plan_session(&facts, Some(9));
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(plan.counts.rejected_splices, 0);
    assert_eq!(plan.counts.ignored_nodes, 0);

    // Following only the first splice must not reach the older thread, which
    // is what makes the recursion load-bearing rather than incidental.
    let mut single = facts.clone();
    single.get_mut(&6).unwrap().summarized_from = None;
    assert_eq!(
        primary_nodes(&plan_session(&single, Some(9))),
        [4, 5, 6, 7, 8, 9]
    );

    // Without any splice the chain is only its own segment.
    let mut bare = facts.clone();
    bare.get_mut(&9).unwrap().summarized_from = None;
    bare.get_mut(&6).unwrap().summarized_from = None;
    assert_eq!(primary_nodes(&plan_session(&bare, Some(9))), [7, 8, 9]);
}

#[test]
fn splice_nodes_are_marked_and_chain_nodes_are_not() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    chain_of(&mut facts, &[0, 3, 4]);
    facts.get_mut(&4).unwrap().summarized_from = Some(2);
    let plan = plan_session(&facts, Some(4));
    let marks = plan.lineages[0]
        .nodes
        .iter()
        .map(|node| (node.node_id, node.splice_kind))
        .collect::<Vec<_>>();
    assert_eq!(
        marks,
        [
            (0, DevinSpliceKind::Chain),
            (1, DevinSpliceKind::Spliced),
            (2, DevinSpliceKind::Spliced),
            (3, DevinSpliceKind::Chain),
            (4, DevinSpliceKind::Chain),
        ]
    );
}

#[test]
fn a_splice_naming_an_absent_node_keeps_the_chain_and_is_counted() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    facts.get_mut(&2).unwrap().summarized_from = Some(99);
    let plan = plan_session(&facts, Some(2));
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2]);
    assert_eq!(plan.counts.rejected_splices, 1);
}

#[test]
fn a_session_without_a_main_chain_yields_metadata_only() {
    let facts = forest(&[(0, None), (1, Some(0))]);
    let plan = plan_session(&facts, None);
    assert_eq!(plan.rejection, Some(DevinSessionRejection::NoMainChain));
    assert!(plan.lineages.is_empty());
    assert_eq!(plan.counts.ignored_nodes, 2);
}

#[test]
fn a_missing_anchor_or_a_hole_mid_chain_rejects_the_session() {
    let facts = forest(&[(0, None), (1, Some(0))]);
    assert_eq!(
        plan_session(&facts, Some(7)).rejection,
        Some(DevinSessionRejection::MissingChainAnchor)
    );

    // Node 2's parent is absent from the session, so the transcript has a hole.
    let holed = forest(&[(2, Some(1)), (3, Some(2))]);
    assert_eq!(
        plan_session(&holed, Some(3)).rejection,
        Some(DevinSessionRejection::BrokenChainParent)
    );
}

#[test]
fn a_parent_that_does_not_precede_its_child_rejects_the_session() {
    // A cycle necessarily violates this, so it is caught before any walk.
    let cyclic = forest(&[(0, Some(1)), (1, Some(0))]);
    assert_eq!(
        plan_session(&cyclic, Some(1)).rejection,
        Some(DevinSessionRejection::NonMonotonicParent)
    );

    let self_loop = forest(&[(0, None), (1, Some(1))]);
    assert_eq!(
        plan_session(&self_loop, Some(1)).rejection,
        Some(DevinSessionRejection::NonMonotonicParent)
    );
}

#[test]
fn a_linked_subagent_becomes_its_own_lineage() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    chain_of(&mut facts, &[10, 11, 12]);
    facts.get_mut(&2).unwrap().subagent_chain_node_id = Some(12);
    facts.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());

    let plan = plan_session(&facts, Some(2));
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(plan.lineages[0].key, DevinLineageKey::Primary);
    assert_eq!(plan.lineages[0].lineage_ord, 0);
    assert_eq!(
        plan.lineages[1].key,
        DevinLineageKey::Subagent("agent-a".to_owned())
    );
    assert_eq!(plan.lineages[1].lineage_ord, 1);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10, 11, 12]
    );
    assert_eq!(plan.counts.ignored_nodes, 0);
    assert_eq!(plan.counts.rejected_lineages, 0);
}

#[test]
fn subagent_links_that_do_not_hold_up_drop_only_that_lineage() {
    // Absent tip.
    let mut absent = BTreeMap::new();
    chain_of(&mut absent, &[0, 1, 2]);
    absent.get_mut(&2).unwrap().subagent_chain_node_id = Some(99);
    absent.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    let plan = plan_session(&absent, Some(2));
    assert_eq!(plan.lineages.len(), 1);
    assert_eq!(plan.counts.rejected_lineages, 1);
    assert_eq!(primary_nodes(&plan), [0, 1, 2]);

    // Blank agent id: no key to attribute the thread to.
    let mut blank = BTreeMap::new();
    chain_of(&mut blank, &[0, 1, 2]);
    chain_of(&mut blank, &[10, 11]);
    blank.get_mut(&2).unwrap().subagent_chain_node_id = Some(11);
    blank.get_mut(&2).unwrap().subagent_agent_id = Some("   ".to_owned());
    let plan = plan_session(&blank, Some(2));
    assert_eq!(plan.lineages.len(), 1);
    assert_eq!(plan.counts.rejected_lineages, 1);

    // Tip inside the primary transcript: not an independent thread.
    let mut overlapping = BTreeMap::new();
    chain_of(&mut overlapping, &[0, 1, 2]);
    overlapping.get_mut(&2).unwrap().subagent_chain_node_id = Some(1);
    overlapping.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    let plan = plan_session(&overlapping, Some(2));
    assert_eq!(plan.lineages.len(), 1);
    assert_eq!(plan.counts.rejected_lineages, 1);
}

#[test]
fn a_repeated_agent_id_keeps_the_first_thread_only() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2, 3]);
    chain_of(&mut facts, &[10, 11]);
    chain_of(&mut facts, &[20, 21]);
    for (linker, tip) in [(2, 11), (3, 21)] {
        facts.get_mut(&linker).unwrap().subagent_chain_node_id = Some(tip);
        facts.get_mut(&linker).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    }
    let plan = plan_session(&facts, Some(3));
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10, 11]
    );
    assert_eq!(plan.counts.rejected_lineages, 1);
    // The second thread stays in the forest, uncounted as a lineage.
    assert_eq!(plan.counts.ignored_nodes, 2);
}

#[test]
fn planning_matches_the_recorded_fixture_sessions() {
    let conn = super::schema_tests::fixture_connection();
    let expected: &[(&str, usize, usize, u64)] = &[
        // session, lineages, primary nodes, ignored nodes
        ("abounding-crest", 1, 11, 20),
        ("exclusive-bamboo", 1, 13, 21),
        ("discovered-sandal", 2, 14, 31),
    ];
    for &(session_id, lineages, primary_len, ignored) in expected {
        let (facts, main_chain_id) = super::stream::read_session_facts(&conn, session_id).unwrap();
        let plan = plan_session(&facts, main_chain_id);
        assert!(plan.rejection.is_none(), "{session_id}");
        assert_eq!(plan.lineages.len(), lineages, "{session_id} lineages");
        assert_eq!(
            plan.lineages[0].nodes.len(),
            primary_len,
            "{session_id} primary length"
        );
        assert_eq!(
            plan.counts.ignored_nodes, ignored,
            "{session_id} ignored nodes"
        );
    }

    // The compaction session is the one that proves the splice matters.
    let (facts, main_chain_id) =
        super::stream::read_session_facts(&conn, "discovered-sandal").unwrap();
    let plan = plan_session(&facts, main_chain_id);
    assert_eq!(
        primary_nodes(&plan),
        [16, 17, 18, 19, 20, 21, 22, 36, 37, 39, 40, 42, 47, 48]
    );
    let spliced = plan.lineages[0]
        .nodes
        .iter()
        .filter(|node| node.splice_kind == DevinSpliceKind::Spliced)
        .count();
    assert_eq!(
        spliced, 9,
        "nine nodes are reachable only through the splice"
    );
    assert_eq!(
        plan.lineages[1].key,
        DevinLineageKey::Subagent("d8a8ea4c".to_owned())
    );
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [31, 32, 33, 35]
    );
}

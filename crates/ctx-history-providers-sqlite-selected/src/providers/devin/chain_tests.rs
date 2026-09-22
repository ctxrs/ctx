use std::collections::BTreeMap;

use super::chain::{
    plan_session, plan_session_with_limits, DevinLineageKey, DevinNodeFacts, DevinSessionPlan,
    DevinSessionRejection, DevinSpliceKind, DevinSubagentHead,
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

fn plan_without_heads(
    facts: &BTreeMap<i64, DevinNodeFacts>,
    main_chain_id: Option<i64>,
) -> DevinSessionPlan {
    plan_session(facts, main_chain_id, &[])
}

#[test]
fn a_linear_chain_is_imported_in_ancestor_first_order() {
    let facts = forest(&[(0, None), (1, Some(0)), (2, Some(1)), (3, Some(2))]);
    let plan = plan_without_heads(&facts, Some(3));
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
    let plan = plan_without_heads(&facts, Some(3));
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

    let plan = plan_without_heads(&facts, Some(9));
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(plan.counts.rejected_splices, 0);
    assert_eq!(plan.counts.ignored_nodes, 0);

    // Following only the first splice must not reach the older thread, which
    // is what makes the recursion load-bearing rather than incidental.
    let mut single = facts.clone();
    single.get_mut(&6).unwrap().summarized_from = None;
    assert_eq!(
        primary_nodes(&plan_without_heads(&single, Some(9))),
        [4, 5, 6, 7, 8, 9]
    );

    // Without any splice the chain is only its own segment.
    let mut bare = facts.clone();
    bare.get_mut(&9).unwrap().summarized_from = None;
    bare.get_mut(&6).unwrap().summarized_from = None;
    assert_eq!(
        primary_nodes(&plan_without_heads(&bare, Some(9))),
        [7, 8, 9]
    );
}

#[test]
fn splice_nodes_are_marked_and_chain_nodes_are_not() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    chain_of(&mut facts, &[0, 3, 4]);
    facts.get_mut(&4).unwrap().summarized_from = Some(2);
    let plan = plan_without_heads(&facts, Some(4));
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
    let plan = plan_without_heads(&facts, Some(2));
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2]);
    assert_eq!(plan.counts.rejected_splices, 1);
}

#[test]
fn a_session_without_a_main_chain_yields_metadata_only() {
    let facts = forest(&[(0, None), (1, Some(0))]);
    let plan = plan_without_heads(&facts, None);
    assert_eq!(plan.rejection, None);
    assert!(plan.lineages.is_empty());
    assert_eq!(plan.counts.ignored_nodes, 2);
}

#[test]
fn a_missing_anchor_or_a_hole_mid_chain_rejects_the_session() {
    let facts = forest(&[(0, None), (1, Some(0))]);
    assert_eq!(
        plan_without_heads(&facts, Some(7)).rejection,
        Some(DevinSessionRejection::MissingChainAnchor)
    );

    // Node 2's parent is absent from the session, so the transcript has a hole.
    let holed = forest(&[(2, Some(1)), (3, Some(2))]);
    assert_eq!(
        plan_without_heads(&holed, Some(3)).rejection,
        Some(DevinSessionRejection::BrokenChainParent)
    );
}

#[test]
fn a_parent_that_does_not_precede_its_child_rejects_the_session() {
    // A cycle necessarily violates this, so it is caught before any walk.
    let cyclic = forest(&[(0, Some(1)), (1, Some(0))]);
    assert_eq!(
        plan_without_heads(&cyclic, Some(1)).rejection,
        Some(DevinSessionRejection::NonMonotonicParent)
    );

    let self_loop = forest(&[(0, None), (1, Some(1))]);
    assert_eq!(
        plan_without_heads(&self_loop, Some(1)).rejection,
        Some(DevinSessionRejection::NonMonotonicParent)
    );
}

#[test]
fn malformed_abandoned_branches_do_not_discard_the_primary_transcript() {
    let facts = forest(&[
        (0, None),
        (1, Some(0)),
        (2, Some(1)),
        // This abandoned cycle is not reachable from the primary transcript.
        (10, Some(11)),
        (11, Some(10)),
    ]);

    let plan = plan_without_heads(&facts, Some(2));
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2]);
    assert_eq!(plan.counts.ignored_nodes, 2);
}

#[test]
fn a_malformed_traversed_subagent_rejects_only_that_lineage() {
    let mut facts = forest(&[
        (0, None),
        (1, Some(0)),
        (2, Some(1)),
        (10, Some(11)),
        (11, Some(10)),
    ]);
    facts.get_mut(&2).unwrap().subagent_chain_node_id = Some(10);
    facts.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());

    let plan = plan_without_heads(&facts, Some(2));
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2]);
    assert_eq!(plan.counts.rejected_lineages, 1);
    assert_eq!(plan.counts.ignored_nodes, 2);
}

#[test]
fn a_subagent_hole_rejects_only_that_lineage() {
    let facts = forest(&[(0, None), (1, Some(0)), (2, Some(1)), (10, Some(99))]);
    let heads = [DevinSubagentHead {
        agent_id: "agent-a".to_owned(),
        chain_node_id: 10,
        updated_at: 0,
    }];

    let plan = plan_session(&facts, Some(2), &heads);
    assert!(plan.rejection.is_none());
    assert_eq!(primary_nodes(&plan), [0, 1, 2]);
    assert_eq!(plan.counts.rejected_lineages, 1);
    assert_eq!(plan.counts.ignored_nodes, 1);
}

#[test]
fn a_linked_subagent_becomes_its_own_lineage() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    chain_of(&mut facts, &[10, 11, 12]);
    facts.get_mut(&2).unwrap().subagent_chain_node_id = Some(12);
    facts.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());

    let plan = plan_without_heads(&facts, Some(2));
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
    let plan = plan_without_heads(&absent, Some(2));
    assert_eq!(plan.lineages.len(), 1);
    assert_eq!(plan.counts.rejected_lineages, 1);
    assert_eq!(primary_nodes(&plan), [0, 1, 2]);

    // Blank agent id: no key to attribute the thread to.
    let mut blank = BTreeMap::new();
    chain_of(&mut blank, &[0, 1, 2]);
    chain_of(&mut blank, &[10, 11]);
    blank.get_mut(&2).unwrap().subagent_chain_node_id = Some(11);
    blank.get_mut(&2).unwrap().subagent_agent_id = Some("   ".to_owned());
    let plan = plan_without_heads(&blank, Some(2));
    assert_eq!(plan.lineages.len(), 1);
    assert_eq!(plan.counts.rejected_lineages, 1);

    // Tip inside the primary transcript: not an independent thread.
    let mut overlapping = BTreeMap::new();
    chain_of(&mut overlapping, &[0, 1, 2]);
    overlapping.get_mut(&2).unwrap().subagent_chain_node_id = Some(1);
    overlapping.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    let plan = plan_without_heads(&overlapping, Some(2));
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
    let plan = plan_without_heads(&facts, Some(3));
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
fn background_heads_follow_foreground_links_in_reader_order() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    for nodes in [[10, 11], [20, 21], [30, 31], [40, 41]] {
        chain_of(&mut facts, &nodes);
    }
    facts.get_mut(&1).unwrap().subagent_chain_node_id = Some(11);
    facts.get_mut(&1).unwrap().subagent_agent_id = Some("agent-z".to_owned());
    facts.get_mut(&2).unwrap().subagent_chain_node_id = Some(21);
    facts.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    let heads = [
        DevinSubagentHead {
            agent_id: "agent-b".to_owned(),
            chain_node_id: 31,
            updated_at: 200,
        },
        DevinSubagentHead {
            agent_id: "agent-c".to_owned(),
            chain_node_id: 41,
            updated_at: 100,
        },
    ];

    let plan = plan_session(&facts, Some(2), &heads);
    assert_eq!(
        plan.lineages
            .iter()
            .map(|lineage| lineage.key.clone())
            .collect::<Vec<_>>(),
        [
            DevinLineageKey::Primary,
            DevinLineageKey::Subagent("agent-z".to_owned()),
            DevinLineageKey::Subagent("agent-a".to_owned()),
            DevinLineageKey::Subagent("agent-b".to_owned()),
            DevinLineageKey::Subagent("agent-c".to_owned()),
        ]
    );
    assert_eq!(plan.counts.ignored_nodes, 0);
    assert_eq!(plan.counts.rejected_lineages, 0);
}

#[test]
fn a_durable_head_extends_the_same_foreground_subagent_chain() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    chain_of(&mut facts, &[10, 11, 12]);
    facts.get_mut(&2).unwrap().subagent_chain_node_id = Some(11);
    facts.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    let heads = [DevinSubagentHead {
        agent_id: "agent-a".to_owned(),
        chain_node_id: 12,
        updated_at: 200,
    }];

    let plan = plan_session(&facts, Some(2), &heads);
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(plan.counts.rejected_lineages, 0);
    assert_eq!(plan.counts.ignored_nodes, 0);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10, 11, 12]
    );
}

#[test]
fn a_compaction_fork_extends_a_persistent_same_agent_lineage() {
    let mut facts = forest(&[
        (0, None),
        (1, Some(0)),
        (2, Some(1)),
        (3, Some(2)),
        (4, Some(3)),
    ]);
    chain_of(&mut facts, &[10, 11, 12, 13, 14, 15, 16, 17, 18, 20]);
    chain_of(&mut facts, &[11, 21, 22, 23]);
    facts.get_mut(&21).unwrap().summarized_from = Some(18);
    for (linker, tip) in [(2, 20), (3, 23), (4, 20)] {
        facts.get_mut(&linker).unwrap().subagent_chain_node_id = Some(tip);
        facts.get_mut(&linker).unwrap().subagent_agent_id = Some("sidekick".to_owned());
    }
    let heads = [DevinSubagentHead {
        agent_id: "sidekick".to_owned(),
        chain_node_id: 23,
        updated_at: 1,
    }];

    let plan = plan_session(&facts, Some(4), &heads);
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(plan.counts.rejected_lineages, 0);
    assert_eq!(plan.counts.rejected_splices, 0);
    assert_eq!(plan.counts.ignored_nodes, 0);
    let sidekick = &plan.lineages[1];
    assert_eq!(
        sidekick.key,
        DevinLineageKey::Subagent("sidekick".to_owned())
    );
    assert_eq!(
        sidekick
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 21, 22, 23]
    );
    assert_eq!(
        sidekick
            .nodes
            .iter()
            .filter(|node| node.splice_kind == DevinSpliceKind::Chain)
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10, 11, 21, 22, 23]
    );
    assert_eq!(
        sidekick
            .nodes
            .iter()
            .filter(|node| node.splice_kind == DevinSpliceKind::Spliced)
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [12, 13, 14, 15, 16, 17, 18, 20]
    );
}

#[test]
fn a_bad_splice_does_not_block_a_durable_head_at_its_valid_ancestor() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0]);
    chain_of(&mut facts, &[10, 30]);
    facts.get_mut(&0).unwrap().subagent_chain_node_id = Some(30);
    facts.get_mut(&0).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    facts.insert(
        20,
        DevinNodeFacts {
            parent_node_id: Some(19),
            ..DevinNodeFacts::default()
        },
    );
    facts.get_mut(&30).unwrap().summarized_from = Some(20);
    let heads = [
        DevinSubagentHead {
            agent_id: "agent-a".to_owned(),
            chain_node_id: 30,
            updated_at: 1,
        },
        DevinSubagentHead {
            agent_id: "agent-a".to_owned(),
            chain_node_id: 30,
            updated_at: 2,
        },
        DevinSubagentHead {
            agent_id: "agent-a".to_owned(),
            chain_node_id: 10,
            updated_at: 3,
        },
    ];

    // Foreground 30 reaches valid ancestor 10, but its splice to 20 ends at
    // absent parent 19. Repeated bad 30 heads use the structural cache; the
    // later durable head at 10 must still be admitted.
    let plan = plan_session_with_limits(&facts, Some(0), &heads, facts.len(), 5);
    assert!(plan.rejection.is_none());
    assert_eq!(plan.counts.rejected_lineages, 3);
    assert_eq!(plan.counts.ignored_nodes, 2);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10]
    );
}

#[test]
fn a_primary_overlap_does_not_block_a_durable_head_at_its_valid_ancestor() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0]);
    chain_of(&mut facts, &[10, 30]);
    facts.get_mut(&0).unwrap().subagent_chain_node_id = Some(30);
    facts.get_mut(&0).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    facts.get_mut(&30).unwrap().summarized_from = Some(0);
    let mut heads = (0..4)
        .map(|updated_at| DevinSubagentHead {
            agent_id: "agent-a".to_owned(),
            chain_node_id: 30,
            updated_at,
        })
        .collect::<Vec<_>>();
    heads.push(DevinSubagentHead {
        agent_id: "agent-b".to_owned(),
        chain_node_id: 10,
        updated_at: 4,
    });

    // Primary 0, the rejected 30 candidate, and valid 10 consume four steps.
    // Repeated 30 heads reject from the overlap cache without spending more.
    let plan = plan_session_with_limits(&facts, Some(0), &heads, facts.len(), 4);
    assert!(plan.rejection.is_none());
    assert_eq!(plan.counts.rejected_lineages, 5);
    assert_eq!(plan.counts.ignored_nodes, 1);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10]
    );
}

#[test]
fn an_oversized_subagent_agent_id_rejects_only_that_lineage() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1]);
    chain_of(&mut facts, &[10, 11]);
    chain_of(&mut facts, &[20, 21]);
    facts.get_mut(&1).unwrap().subagent_chain_node_id = Some(11);
    facts.get_mut(&1).unwrap().subagent_agent_id =
        Some("x".repeat(super::chain::DEVIN_SUBAGENT_AGENT_ID_BYTES + 1));
    let heads = [DevinSubagentHead {
        agent_id: "agent-a".to_owned(),
        chain_node_id: 21,
        updated_at: 0,
    }];

    let plan = plan_session(&facts, Some(1), &heads);
    assert!(plan.rejection.is_none());
    assert_eq!(plan.counts.rejected_lineages, 1);
    assert_eq!(plan.counts.ignored_nodes, 2);
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [20, 21]
    );
}

#[test]
fn an_invalid_first_tip_does_not_hide_a_valid_head_for_the_same_agent() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    chain_of(&mut facts, &[10, 11, 12]);
    facts.get_mut(&2).unwrap().subagent_chain_node_id = Some(99);
    facts.get_mut(&2).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    let heads = [DevinSubagentHead {
        agent_id: "agent-a".to_owned(),
        chain_node_id: 12,
        updated_at: 200,
    }];

    let plan = plan_session(&facts, Some(2), &heads);
    assert_eq!(plan.counts.rejected_lineages, 1);
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        [10, 11, 12]
    );
}

#[test]
fn durable_duplicates_are_noops_and_bad_heads_reject_locally() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0, 1, 2]);
    for nodes in [[10, 11], [20, 21], [30, 31]] {
        chain_of(&mut facts, &nodes);
    }
    facts.get_mut(&1).unwrap().subagent_chain_node_id = Some(11);
    facts.get_mut(&1).unwrap().subagent_agent_id = Some("agent-a".to_owned());
    facts.get_mut(&2).unwrap().subagent_chain_node_id = Some(21);
    facts.get_mut(&2).unwrap().subagent_agent_id = Some("agent-b".to_owned());
    let heads = [
        // Exact foreground duplicate: neither another lineage nor a rejection.
        DevinSubagentHead {
            agent_id: "agent-a".to_owned(),
            chain_node_id: 11,
            updated_at: 1,
        },
        // Same exact agent, different tip.
        DevinSubagentHead {
            agent_id: "agent-b".to_owned(),
            chain_node_id: 31,
            updated_at: 2,
        },
        DevinSubagentHead {
            agent_id: "  ".to_owned(),
            chain_node_id: 31,
            updated_at: 3,
        },
        DevinSubagentHead {
            agent_id: "agent-missing".to_owned(),
            chain_node_id: 99,
            updated_at: 4,
        },
        DevinSubagentHead {
            agent_id: "agent-overlap".to_owned(),
            chain_node_id: 1,
            updated_at: 5,
        },
    ];

    let plan = plan_session(&facts, Some(2), &heads);
    assert_eq!(plan.lineages.len(), 3);
    assert_eq!(plan.counts.rejected_lineages, 4);
    assert_eq!(plan.counts.ignored_nodes, 2);
    assert_eq!(
        plan.lineages
            .iter()
            .map(|lineage| lineage.key.clone())
            .collect::<Vec<_>>(),
        [
            DevinLineageKey::Primary,
            DevinLineageKey::Subagent("agent-a".to_owned()),
            DevinLineageKey::Subagent("agent-b".to_owned()),
        ]
    );
}

#[test]
fn overlapping_durable_heads_share_one_walk_budget() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0]);
    chain_of(&mut facts, &[10, 11, 12, 13, 14, 15, 16, 17, 18]);
    let heads = (0..32)
        .map(|index| DevinSubagentHead {
            agent_id: format!("agent-{index:02}"),
            chain_node_id: 18,
            updated_at: index,
        })
        .collect::<Vec<_>>();

    // One step for the primary and nine for the first accepted head. Every
    // later head must reject from the shared ownership index without another
    // walk.
    let plan = plan_session_with_limits(&facts, Some(0), &heads, facts.len(), facts.len());
    assert!(plan.rejection.is_none());
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(plan.counts.rejected_lineages, 31);
    assert_eq!(plan.counts.ignored_nodes, 0);
}

#[test]
fn repeated_rejected_overlaps_do_not_starve_a_later_valid_head() {
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &[0]);
    chain_of(&mut facts, &(10..1_010).collect::<Vec<_>>());
    facts.get_mut(&10).unwrap().parent_node_id = Some(0);
    chain_of(&mut facts, &[2_000, 2_001]);
    let mut heads = (0..4_000)
        .map(|index| DevinSubagentHead {
            agent_id: format!("overlap-{index:04}"),
            chain_node_id: 1_009,
            updated_at: index,
        })
        .collect::<Vec<_>>();
    heads.push(DevinSubagentHead {
        agent_id: "valid".to_owned(),
        chain_node_id: 2_001,
        updated_at: 4_001,
    });

    // Primary + one rejected overlap walk + the independent valid chain.
    let plan = plan_session_with_limits(&facts, Some(0), &heads, facts.len(), 1_003);
    assert!(plan.rejection.is_none());
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(
        plan.lineages[1].key,
        DevinLineageKey::Subagent("valid".to_owned())
    );
    assert_eq!(plan.counts.rejected_lineages, 4_000);
}

#[test]
fn successive_same_agent_tips_walk_only_the_advancing_suffix() {
    const NODES: i64 = 3_000;
    let mut facts = BTreeMap::new();
    chain_of(&mut facts, &(0..NODES).collect::<Vec<_>>());
    chain_of(&mut facts, &(NODES..NODES * 2).collect::<Vec<_>>());
    for node_id in 0..NODES {
        let node = facts.get_mut(&node_id).unwrap();
        node.subagent_chain_node_id = Some(NODES + node_id);
        node.subagent_agent_id = Some("agent-a".to_owned());
    }

    // The primary and final subagent lineage each consume 3,000 steps. A
    // repeated full-root comparison would exhaust this exact 6,000-step bound.
    let plan = plan_session_with_limits(&facts, Some(NODES - 1), &[], facts.len(), facts.len());
    assert!(plan.rejection.is_none());
    assert_eq!(plan.lineages.len(), 2);
    assert_eq!(plan.counts.rejected_lineages, 0);
    assert_eq!(plan.counts.ignored_nodes, 0);
    assert_eq!(
        plan.lineages[1]
            .nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        (NODES..NODES * 2).collect::<Vec<_>>()
    );
}

#[test]
fn nested_links_walk_each_node_once_without_recursion() {
    const DEPTH: i64 = 2_000;
    let facts = (0..=DEPTH)
        .map(|id| {
            (
                id,
                DevinNodeFacts {
                    subagent_chain_node_id: (id < DEPTH).then_some(id + 1),
                    subagent_agent_id: (id < DEPTH).then(|| format!("agent-{}", id + 1)),
                    ..DevinNodeFacts::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let plan = plan_session_with_limits(&facts, Some(0), &[], facts.len(), facts.len());
    assert!(plan.rejection.is_none());
    assert_eq!(plan.lineages.len(), facts.len());
    assert_eq!(plan.counts.ignored_nodes, 0);
    assert_eq!(plan.counts.rejected_lineages, 0);
    assert_eq!(
        plan.lineages.last().unwrap().parent_key,
        Some(DevinLineageKey::Subagent(format!("agent-{}", DEPTH - 1)))
    );
}

#[test]
fn durable_head_reader_preserves_values_and_orders_by_agent_id() {
    let (_temp, conn) = super::schema_tests::mutable_fixture();
    for (session_id, agent_id, chain_node_id, updated_at) in [
        ("discovered-sandal", "agent-z", 41, 900),
        ("discovered-sandal", "agent-a ", 31, 700),
        ("abounding-crest", "other-session", 11, 800),
    ] {
        conn.execute(
            "insert into subagent_heads(session_id, agent_id, chain_node_id, updated_at) \
             values (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, agent_id, chain_node_id, updated_at],
        )
        .unwrap();
    }

    let heads = super::stream::read_subagent_heads(&conn, "discovered-sandal").unwrap();
    assert_eq!(
        heads,
        [
            DevinSubagentHead {
                agent_id: "agent-a ".to_owned(),
                chain_node_id: 31,
                updated_at: 700,
            },
            DevinSubagentHead {
                agent_id: "agent-z".to_owned(),
                chain_node_id: 41,
                updated_at: 900,
            },
        ]
    );
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
        let plan = plan_without_heads(&facts, main_chain_id);
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
    let plan = plan_without_heads(&facts, main_chain_id);
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

//! Devin transcript planning over the per-session message forest.
//!
//! `message_nodes` is a forest, not a list. A session records one
//! `main_chain_id`, and the transcript is the walk from it up `parent_node_id`.
//! Compaction complicates that: when Devin compacts, it writes a fresh chain
//! and marks the continuation node with `metadata.summarized_from`, naming the
//! node whose history it replaced. The prior history is reachable only by
//! walking from that node, and a spliced segment can itself have been
//! compacted, so the splice is transitive.
//!
//! Everything else in the forest is deliberately not imported: summarizer
//! threads, abandoned regenerated turns, and the pre-compaction copies Devin
//! rewrote. Those are counted so a source with unexpected shape is visible
//! rather than silently partial.
//!
//! # Ordering
//!
//! Planning collects a node set and emits it in `node_id` order. Devin always
//! assigns a child a higher `node_id` than its parent, so that order is
//! ancestor-first by construction; the invariant is checked rather than
//! assumed, and a session that violates it is rejected. `node_id` is preferred
//! over `created_at` because `created_at` has one-second granularity and ties
//! across most of a session, and preferred over parent-walk order because a
//! walk emits a post-compaction prefix ahead of the older history it
//! continues.
//!
//! Ordering does not need the private scratch database that other providers in
//! this pack use. Devin's own `UNIQUE(session_id, node_id)` index already
//! yields ordered reads and single-node probes without SQLite sorting in
//! temporary storage, so there is nothing to materialize.

use std::collections::{BTreeMap, BTreeSet};

/// Which transcript a planned node belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DevinLineageKey {
    Primary,
    /// A subagent thread Devin explicitly back-linked, keyed by its agent id.
    Subagent(String),
}

/// Whether a node was on the recorded chain or recovered through a splice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DevinSpliceKind {
    Chain,
    Spliced,
}

impl DevinSpliceKind {
    pub(super) fn code(self) -> u8 {
        match self {
            Self::Chain => 0,
            Self::Spliced => 1,
        }
    }
}

/// The per-node facts planning needs, read without transferring payloads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DevinNodeFacts {
    pub(super) parent_node_id: Option<i64>,
    pub(super) summarized_from: Option<i64>,
    pub(super) subagent_chain_node_id: Option<i64>,
    pub(super) subagent_agent_id: Option<String>,
}

/// Why a session yielded no transcript at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DevinSessionRejection {
    /// The session records no main chain, so it has metadata only.
    NoMainChain,
    /// `main_chain_id` names a node the session does not contain.
    MissingChainAnchor,
    /// A chain node's `parent_node_id` names a node the session does not
    /// contain, so the transcript has a hole rather than an end.
    BrokenChainParent,
    /// A parent does not precede its child, so ancestor-first order cannot be
    /// established from `node_id`.
    NonMonotonicParent,
    /// The forest exceeded the per-session planning bound.
    TooManyNodes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinPlannedNode {
    pub(super) node_id: i64,
    pub(super) chain_ord: u32,
    pub(super) splice_kind: DevinSpliceKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinLineagePlan {
    pub(super) key: DevinLineageKey,
    pub(super) lineage_ord: u32,
    pub(super) nodes: Vec<DevinPlannedNode>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DevinPlanCounts {
    /// Nodes present in the forest but outside every imported lineage.
    pub(super) ignored_nodes: u64,
    /// Subagent lineages dropped because their link did not hold up.
    pub(super) rejected_lineages: u64,
    /// Splices dropped because the node they named is absent.
    pub(super) rejected_splices: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinSessionPlan {
    pub(super) lineages: Vec<DevinLineagePlan>,
    pub(super) counts: DevinPlanCounts,
    /// Set when the session yields no transcript; the reason is recorded so a
    /// rejection is distinguishable from an empty session.
    pub(super) rejection: Option<DevinSessionRejection>,
}

impl DevinSessionPlan {
    fn rejected(reason: DevinSessionRejection, ignored_nodes: u64) -> Self {
        Self {
            lineages: Vec::new(),
            counts: DevinPlanCounts {
                ignored_nodes,
                ..DevinPlanCounts::default()
            },
            rejection: Some(reason),
        }
    }

    pub(super) fn imported_nodes(&self) -> u64 {
        self.lineages
            .iter()
            .map(|lineage| lineage.nodes.len() as u64)
            .sum()
    }
}

/// The largest forest planning will walk before refusing the session.
///
/// Planning holds one entry per node, so this is what bounds its memory. The
/// largest session observed in a real store held a few thousand nodes; this
/// leaves several orders of magnitude of headroom while staying finite.
pub(super) const DEVIN_MAX_SESSION_NODES: usize = 1_000_000;

/// Plans one session's transcript from its forest.
///
/// `facts` must contain every node in the session.
pub(super) fn plan_session(
    facts: &BTreeMap<i64, DevinNodeFacts>,
    main_chain_id: Option<i64>,
) -> DevinSessionPlan {
    let total = facts.len() as u64;
    if facts.len() > DEVIN_MAX_SESSION_NODES {
        return DevinSessionPlan::rejected(DevinSessionRejection::TooManyNodes, total);
    }
    // Ancestor-first ordering rests on this, so it is verified once up front
    // rather than trusted per walk.
    if let Some((&node_id, _)) = facts.iter().find(|(node_id, node)| {
        node.parent_node_id
            .is_some_and(|parent| parent >= **node_id)
    }) {
        let _ = node_id;
        return DevinSessionPlan::rejected(DevinSessionRejection::NonMonotonicParent, total);
    }
    let Some(main_chain_id) = main_chain_id else {
        return DevinSessionPlan::rejected(DevinSessionRejection::NoMainChain, total);
    };
    if !facts.contains_key(&main_chain_id) {
        return DevinSessionPlan::rejected(DevinSessionRejection::MissingChainAnchor, total);
    }

    let mut counts = DevinPlanCounts::default();
    let mut claimed = BTreeSet::<i64>::new();

    let primary = match collect_lineage(facts, main_chain_id, &mut counts, &claimed) {
        Ok(nodes) => nodes,
        Err(reason) => return DevinSessionPlan::rejected(reason, total),
    };
    claimed.extend(primary.iter().copied());

    let mut lineages = vec![DevinLineagePlan {
        key: DevinLineageKey::Primary,
        lineage_ord: 0,
        nodes: ordered_nodes(facts, &primary, main_chain_id),
    }];

    // Subagent threads are imported only where the primary transcript itself
    // links to them, and only once per agent id.
    let mut seen_agents = BTreeSet::<String>::new();
    for node_id in &primary {
        let node = &facts[node_id];
        let (Some(tip), Some(agent_id)) = (
            node.subagent_chain_node_id,
            node.subagent_agent_id.as_deref(),
        ) else {
            continue;
        };
        if agent_id.trim().is_empty() || !seen_agents.insert(agent_id.to_owned()) {
            counts.rejected_lineages += 1;
            continue;
        }
        if !facts.contains_key(&tip) {
            counts.rejected_lineages += 1;
            continue;
        }
        let mut lineage_counts = DevinPlanCounts::default();
        let Ok(nodes) = collect_lineage(facts, tip, &mut lineage_counts, &claimed) else {
            counts.rejected_lineages += 1;
            continue;
        };
        // A subagent thread that reaches into an already-imported lineage is
        // not an independent transcript, so no lineage claim is made for it.
        if nodes.iter().any(|node_id| claimed.contains(node_id)) {
            counts.rejected_lineages += 1;
            continue;
        }
        counts.rejected_splices += lineage_counts.rejected_splices;
        claimed.extend(nodes.iter().copied());
        let lineage_ord = lineages.len() as u32;
        lineages.push(DevinLineagePlan {
            key: DevinLineageKey::Subagent(agent_id.to_owned()),
            lineage_ord,
            nodes: ordered_nodes(facts, &nodes, tip),
        });
    }

    counts.ignored_nodes = total - claimed.len() as u64;
    DevinSessionPlan {
        lineages,
        counts,
        rejection: None,
    }
}

/// Walks one lineage from `tip`, following parents and splicing transitively.
///
/// `claimed` is only consulted to stop a walk re-entering another lineage; the
/// returned set may still overlap it, which the caller treats as a rejection.
fn collect_lineage(
    facts: &BTreeMap<i64, DevinNodeFacts>,
    tip: i64,
    counts: &mut DevinPlanCounts,
    claimed: &BTreeSet<i64>,
) -> Result<BTreeSet<i64>, DevinSessionRejection> {
    let mut collected = BTreeSet::<i64>::new();
    // Anchors still to expand. Bounded by the node count because an anchor is
    // only ever pushed when its node is first reached.
    let mut pending = vec![tip];
    while let Some(anchor) = pending.pop() {
        let mut cursor = Some(anchor);
        let mut steps = 0_usize;
        while let Some(node_id) = cursor {
            let Some(node) = facts.get(&node_id) else {
                return Err(DevinSessionRejection::BrokenChainParent);
            };
            steps += 1;
            if steps > facts.len() {
                // Unreachable while parents strictly decrease, which is
                // verified before planning; kept so the walk is bounded on its
                // own terms rather than by a caller's invariant.
                return Err(DevinSessionRejection::BrokenChainParent);
            }
            let first_visit = collected.insert(node_id);
            if first_visit && !claimed.contains(&node_id) {
                if let Some(summarized_from) = node.summarized_from {
                    if facts.contains_key(&summarized_from) {
                        pending.push(summarized_from);
                    } else {
                        // The summary node and its descendants stay; only the
                        // recovered history is lost.
                        counts.rejected_splices += 1;
                    }
                }
            }
            if !first_visit {
                // Already walked this ancestry on an earlier anchor.
                break;
            }
            cursor = node.parent_node_id;
        }
    }
    Ok(collected)
}

/// Orders a lineage's nodes and marks which were on the recorded chain.
fn ordered_nodes(
    facts: &BTreeMap<i64, DevinNodeFacts>,
    nodes: &BTreeSet<i64>,
    tip: i64,
) -> Vec<DevinPlannedNode> {
    let on_chain = walk_to_root(facts, tip);
    nodes
        .iter()
        .enumerate()
        .map(|(index, &node_id)| DevinPlannedNode {
            node_id,
            chain_ord: index as u32,
            splice_kind: if on_chain.contains(&node_id) {
                DevinSpliceKind::Chain
            } else {
                DevinSpliceKind::Spliced
            },
        })
        .collect()
}

fn walk_to_root(facts: &BTreeMap<i64, DevinNodeFacts>, tip: i64) -> BTreeSet<i64> {
    let mut out = BTreeSet::new();
    let mut cursor = Some(tip);
    while let Some(node_id) = cursor {
        if !out.insert(node_id) {
            break;
        }
        cursor = facts.get(&node_id).and_then(|node| node.parent_node_id);
    }
    out
}

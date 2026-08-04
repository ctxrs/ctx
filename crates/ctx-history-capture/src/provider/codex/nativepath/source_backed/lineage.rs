use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use sha2::{Digest, Sha256};

use super::*;
use crate::provider::codex::nativepath::reader::CodexLineageFactPresenceV0;

const LINEAGE_DEPENDENCY_DOMAIN: &[u8] = b"ctx/codex-lineage-dependency/v2\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexOutcomeOriginV0 {
    UniqueToSession,
    CopiedFromAncestor,
    Unproven,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParentLinkV0 {
    Root,
    Source(usize),
    Missing(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationshipStateV0 {
    Root,
    Acyclic,
    Missing,
    Cycle,
}

#[derive(Debug)]
struct LineageNodeV0 {
    native_session_id: String,
    observation: CodexFileObservation,
    parent: ParentLinkV0,
    has_children: bool,
    dependency_digest: [u8; 32],
    relationship_state: RelationshipStateV0,
    depth: usize,
}

#[derive(Debug)]
enum LineageFactsStateV0 {
    Pending,
    OutsideRoute,
    CompleteLeaf,
    Ready(CodexLineageFactsV0),
}

#[derive(Debug)]
pub(super) struct CodexOutcomeLineageAuthorityV0 {
    nodes: Vec<LineageNodeV0>,
    indices: HashMap<String, usize>,
    facts: Mutex<Vec<LineageFactsStateV0>>,
    budget: Arc<CodexLineageFactBudgetV0>,
    #[cfg(test)]
    dependency_work_units: usize,
}

impl CodexOutcomeLineageAuthorityV0 {
    pub(super) fn from_sources(
        sources: &[(CodexCatalogSource, SourceKey, String)],
    ) -> CodexSourceBackedResultV0<Self> {
        Self::from_sources_with_budget(sources, Arc::new(CodexLineageFactBudgetV0::default()))
    }

    fn from_sources_with_budget(
        sources: &[(CodexCatalogSource, SourceKey, String)],
        budget: Arc<CodexLineageFactBudgetV0>,
    ) -> CodexSourceBackedResultV0<Self> {
        let mut indices = HashMap::new();
        indices
            .try_reserve(sources.len())
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
        for (index, (_, _, native_session_id)) in sources.iter().enumerate() {
            if indices.insert(native_session_id.clone(), index).is_some() {
                return Err(CodexSourceBackedErrorV0::DuplicateNativeSessionId(
                    native_session_id.clone(),
                ));
            }
        }

        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(sources.len())
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
        for (source, _, native_session_id) in sources {
            let parent = match source.catalog_parent_native_session_id.as_ref() {
                None => ParentLinkV0::Root,
                Some(parent) => indices
                    .get(parent)
                    .copied()
                    .map(ParentLinkV0::Source)
                    .unwrap_or_else(|| ParentLinkV0::Missing(parent.clone())),
            };
            nodes.push(LineageNodeV0 {
                native_session_id: native_session_id.clone(),
                observation: source.catalog_observation.clone(),
                parent,
                has_children: false,
                dependency_digest: [0; 32],
                relationship_state: RelationshipStateV0::Root,
                depth: 0,
            });
        }
        for index in 0..nodes.len() {
            let parent = match nodes[index].parent {
                ParentLinkV0::Source(parent) => Some(parent),
                ParentLinkV0::Root | ParentLinkV0::Missing(_) => None,
            };
            if let Some(parent) = parent {
                nodes
                    .get_mut(parent)
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
                    .has_children = true;
            }
        }
        let dependency_work_units = compute_dependency_digests(&mut nodes)?;
        #[cfg(not(test))]
        let _ = dependency_work_units;

        let mut facts = Vec::new();
        facts
            .try_reserve_exact(nodes.len())
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
        facts.resize_with(nodes.len(), || LineageFactsStateV0::Pending);
        Ok(Self {
            nodes,
            indices,
            facts: Mutex::new(facts),
            budget,
            #[cfg(test)]
            dependency_work_units,
        })
    }

    #[cfg(test)]
    pub(super) fn unscoped() -> Self {
        Self {
            nodes: Vec::new(),
            indices: HashMap::new(),
            facts: Mutex::new(Vec::new()),
            budget: Arc::new(CodexLineageFactBudgetV0::default()),
            dependency_work_units: 0,
        }
    }

    pub(super) fn new_fact_set(&self) -> CodexSourceBackedResultV0<CodexLineageFactsV0> {
        CodexLineageFactsV0::new(Arc::clone(&self.budget)).map_err(map_lineage_capture_error)
    }

    pub(super) fn bind_route_sources(
        &self,
        selected_native_session_ids: &HashSet<String>,
    ) -> CodexSourceBackedResultV0<()> {
        let mut facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        for (node, state) in self.nodes.iter().zip(facts.iter_mut()) {
            if !selected_native_session_ids.contains(&node.native_session_id) {
                if !matches!(state, LineageFactsStateV0::Pending) {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
                }
                *state = LineageFactsStateV0::OutsideRoute;
            }
        }
        Ok(())
    }

    pub(super) fn register(
        &self,
        native_session_id: &str,
        mut facts: CodexLineageFactsV0,
    ) -> CodexSourceBackedResultV0<()> {
        facts.seal();
        let index = self
            .indices
            .get(native_session_id)
            .copied()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let retain_facts = self
            .nodes
            .get(index)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
            .has_children;
        let mut registered = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let slot = registered
            .get_mut(index)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        match slot {
            LineageFactsStateV0::Pending => {
                *slot = if retain_facts {
                    LineageFactsStateV0::Ready(facts)
                } else {
                    // A session's facts are consulted only while classifying
                    // descendants. Terminal leaves still need a completed
                    // state, but retaining their facts can never affect an
                    // outcome and would turn corpus size into live state.
                    LineageFactsStateV0::CompleteLeaf
                };
                Ok(())
            }
            LineageFactsStateV0::OutsideRoute
            | LineageFactsStateV0::CompleteLeaf
            | LineageFactsStateV0::Ready(_) => {
                Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
            }
        }
    }

    pub(super) fn dependency_digest(&self, native_session_id: &str) -> [u8; 32] {
        self.indices
            .get(native_session_id)
            .and_then(|index| self.nodes.get(*index))
            .map(|node| node.dependency_digest)
            .unwrap_or_else(|| digest_marker(b"unknown-source\0"))
    }

    pub(super) fn depth(&self, native_session_id: &str) -> usize {
        self.indices
            .get(native_session_id)
            .and_then(|index| self.nodes.get(*index))
            .map_or(usize::MAX, |node| node.depth)
    }

    pub(super) fn component_affinity(&self, native_session_id: &str) -> u64 {
        let Some(mut current) = self.indices.get(native_session_id).copied() else {
            return u64::MAX;
        };
        let mut remaining = self.nodes.len().saturating_add(1);
        while remaining != 0 {
            remaining = remaining.saturating_sub(1);
            let Some(node) = self.nodes.get(current) else {
                return u64::MAX;
            };
            if node.depth == 0 {
                let digest: [u8; 32] = if node.relationship_state == RelationshipStateV0::Cycle {
                    node.dependency_digest
                } else {
                    let mut hasher = dependency_hasher(b"component\0");
                    hash_text(&mut hasher, &node.native_session_id);
                    hasher.finalize().into()
                };
                return u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8]));
            }
            match node.parent {
                ParentLinkV0::Source(parent) => current = parent,
                ParentLinkV0::Root | ParentLinkV0::Missing(_) => return u64::MAX,
            }
        }
        u64::MAX
    }

    pub(super) fn classify(
        &self,
        native_session_id: &str,
        origin_call_id: &str,
        result_call_id: &str,
        origin_occurred_at_unix_ms: Option<i64>,
        result_occurred_at_unix_ms: i64,
        session_started_at_unix_ms: i64,
    ) -> CodexSourceBackedResultV0<CodexOutcomeOriginV0> {
        let Some(current) = self
            .indices
            .get(native_session_id)
            .and_then(|index| self.nodes.get(*index))
        else {
            return Ok(CodexOutcomeOriginV0::Unproven);
        };
        if current.relationship_state == RelationshipStateV0::Cycle {
            return Ok(CodexOutcomeOriginV0::Unproven);
        }
        // A Codex fork snapshots its parent when the child session starts. An
        // exact invocation/result pair recorded strictly after that boundary
        // cannot have been copied from the parent, even when an older ancestor
        // archive is unavailable. Mismatched, incomplete, or pre-fork evidence
        // continues through the fail-closed ancestor-presence proof below.
        if exact_correlated_result_postdates_fork(
            origin_call_id,
            result_call_id,
            origin_occurred_at_unix_ms,
            result_occurred_at_unix_ms,
            session_started_at_unix_ms,
        ) {
            return Ok(CodexOutcomeOriginV0::UniqueToSession);
        }
        let facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut parent = current.parent.clone();
        let mut remaining = self.nodes.len().saturating_add(1);
        while remaining != 0 {
            remaining = remaining.saturating_sub(1);
            let parent_index = match parent {
                ParentLinkV0::Root => return Ok(CodexOutcomeOriginV0::UniqueToSession),
                ParentLinkV0::Missing(_) => return Ok(CodexOutcomeOriginV0::Unproven),
                ParentLinkV0::Source(index) => index,
            };
            let parent_node = self
                .nodes
                .get(parent_index)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            let parent_facts = match facts.get(parent_index) {
                Some(LineageFactsStateV0::Ready(facts)) => facts,
                Some(LineageFactsStateV0::OutsideRoute) => {
                    return Ok(CodexOutcomeOriginV0::Unproven)
                }
                Some(LineageFactsStateV0::CompleteLeaf) => {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
                }
                Some(LineageFactsStateV0::Pending) | None => {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
                }
            };
            match parent_facts.presence(origin_call_id, result_call_id) {
                CodexLineageFactPresenceV0::Present => {
                    return Ok(CodexOutcomeOriginV0::CopiedFromAncestor)
                }
                CodexLineageFactPresenceV0::Unproven => return Ok(CodexOutcomeOriginV0::Unproven),
                CodexLineageFactPresenceV0::Absent => {}
            }
            if parent_node.relationship_state == RelationshipStateV0::Cycle {
                return Ok(CodexOutcomeOriginV0::Unproven);
            }
            parent = parent_node.parent.clone();
        }
        Ok(CodexOutcomeOriginV0::Unproven)
    }

    #[cfg(test)]
    fn poison_facts_lock(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.facts.lock().unwrap_or_else(|error| error.into_inner());
            panic!("poison Codex lineage facts lock");
        }));
    }
}

fn exact_correlated_result_postdates_fork(
    origin_call_id: &str,
    result_call_id: &str,
    origin_occurred_at_unix_ms: Option<i64>,
    result_occurred_at_unix_ms: i64,
    session_started_at_unix_ms: i64,
) -> bool {
    !origin_call_id.is_empty()
        && origin_call_id == result_call_id
        && origin_occurred_at_unix_ms
            .is_some_and(|occurred_at| occurred_at > session_started_at_unix_ms)
        && result_occurred_at_unix_ms > session_started_at_unix_ms
}

fn compute_dependency_digests(nodes: &mut [LineageNodeV0]) -> CodexSourceBackedResultV0<usize> {
    let mut colors = Vec::new();
    colors
        .try_reserve_exact(nodes.len())
        .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
    colors.resize(nodes.len(), 0_u8);
    let mut work_units = 0_usize;

    for start in 0..nodes.len() {
        if colors[start] == 2 {
            continue;
        }
        let mut path = Vec::new();
        path.try_reserve_exact(64)
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
        let mut current = start;
        loop {
            if colors[current] == 0 {
                colors[current] = 1;
                if path.len() == path.capacity() {
                    path.try_reserve_exact(64)
                        .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
                }
                path.push(current);
                match nodes[current].parent {
                    ParentLinkV0::Source(parent) => {
                        work_units = work_units.saturating_add(1);
                        current = parent;
                        continue;
                    }
                    ParentLinkV0::Root => {
                        nodes[current].dependency_digest = digest_marker(b"root\0");
                        nodes[current].relationship_state = RelationshipStateV0::Root;
                        nodes[current].depth = 0;
                        colors[current] = 2;
                    }
                    ParentLinkV0::Missing(ref parent) => {
                        let mut hasher = dependency_hasher(b"missing\0");
                        hash_text(&mut hasher, parent);
                        nodes[current].dependency_digest = hasher.finalize().into();
                        nodes[current].relationship_state = RelationshipStateV0::Missing;
                        nodes[current].depth = 0;
                        colors[current] = 2;
                    }
                }
            } else if colors[current] == 1 {
                let mut cycle_start = None;
                for (position, candidate) in path.iter().enumerate() {
                    work_units = work_units.saturating_add(1);
                    if *candidate == current {
                        cycle_start = Some(position);
                        break;
                    }
                }
                let cycle_start =
                    cycle_start.ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
                let cycle = &path[cycle_start..];
                let mut canonical = *cycle
                    .first()
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
                for index in &cycle[1..] {
                    work_units = work_units.saturating_add(1);
                    if nodes[*index].native_session_id < nodes[canonical].native_session_id {
                        canonical = *index;
                    }
                }
                let mut hasher = dependency_hasher(b"cycle\0");
                let mut cycle_index = canonical;
                for _ in 0..cycle.len() {
                    work_units = work_units.saturating_add(1);
                    hash_text(&mut hasher, &nodes[cycle_index].native_session_id);
                    hash_observation(&mut hasher, &nodes[cycle_index].observation);
                    let ParentLinkV0::Source(parent) = nodes[cycle_index].parent else {
                        return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
                    };
                    cycle_index = parent;
                }
                if cycle_index != canonical {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
                }
                let cycle_digest: [u8; 32] = hasher.finalize().into();
                for index in cycle {
                    work_units = work_units.saturating_add(1);
                    nodes[*index].dependency_digest = cycle_digest;
                    nodes[*index].relationship_state = RelationshipStateV0::Cycle;
                    nodes[*index].depth = 0;
                    colors[*index] = 2;
                }
            }
            break;
        }

        for index in path.into_iter().rev() {
            work_units = work_units.saturating_add(1);
            if colors[index] == 2 {
                continue;
            }
            let ParentLinkV0::Source(parent) = nodes[index].parent else {
                return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
            };
            if colors[parent] != 2 {
                return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
            }
            let mut hasher = dependency_hasher(b"edge\0");
            hash_text(&mut hasher, &nodes[parent].native_session_id);
            hash_observation(&mut hasher, &nodes[parent].observation);
            hasher.update(nodes[parent].dependency_digest);
            hasher.update([nodes[parent].relationship_state as u8]);
            nodes[index].dependency_digest = hasher.finalize().into();
            nodes[index].relationship_state = match nodes[parent].relationship_state {
                RelationshipStateV0::Root | RelationshipStateV0::Acyclic => {
                    RelationshipStateV0::Acyclic
                }
                RelationshipStateV0::Missing => RelationshipStateV0::Missing,
                RelationshipStateV0::Cycle => RelationshipStateV0::Cycle,
            };
            nodes[index].depth = nodes[parent].depth.saturating_add(1);
            colors[index] = 2;
        }
    }
    Ok(work_units)
}

fn dependency_hasher(marker: &[u8]) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(LINEAGE_DEPENDENCY_DOMAIN);
    hasher.update(marker);
    hasher
}

fn digest_marker(marker: &[u8]) -> [u8; 32] {
    dependency_hasher(marker).finalize().into()
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_observation(hasher: &mut Sha256, observation: &CodexFileObservation) {
    hasher.update(observation.len.to_le_bytes());
    hasher.update(observation.modified_at_ms.to_le_bytes());
    match observation.stable_token {
        Some(token) => {
            hasher.update([1]);
            hasher.update(token);
        }
        None => hasher.update([0]),
    }
    hasher.update(observation.change_token);
}

pub(super) fn map_lineage_capture_error(error: CaptureError) -> CodexSourceBackedErrorV0 {
    match &error {
        CaptureError::InvalidPayload(detail) if detail == CODEX_LINEAGE_EXHAUSTED_SENTINEL => {
            CodexSourceBackedErrorV0::LineageWorkingSetExhausted
        }
        _ => CodexSourceBackedErrorV0::Capture(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                catalog_root_native_session_id: None,
                opened: None,
                authority_root: None,
                authority_relative_path: None,
            },
            source_key,
            id.to_owned(),
        )
    }

    #[test]
    fn dependency_digest_walk_is_linear_for_deep_chain() {
        let mut sources = Vec::new();
        for index in 0..4096_usize {
            let id = format!("node-{index}");
            let parent = (index != 0).then(|| format!("node-{}", index - 1));
            sources.push(source(&id, parent.as_deref(), (index % 251) as u8));
        }
        let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
        assert!(authority.dependency_work_units <= sources.len().saturating_mul(3));
        assert_ne!(authority.dependency_digest("node-4095"), [0; 32]);
    }

    #[test]
    fn dependency_digest_walk_is_linear_and_canonical_for_one_large_cycle() {
        const NODES: usize = 4096;
        let mut sources = Vec::new();
        for index in 0..NODES {
            let id = format!("cycle-{index:04}");
            let parent = format!("cycle-{:04}", (index + 1) % NODES);
            sources.push(source(&id, Some(&parent), (index % 251) as u8));
        }
        let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
        assert!(authority.dependency_work_units <= NODES.saturating_mul(7));
        let expected = authority.dependency_digest("cycle-0000");
        assert_ne!(expected, [0; 32]);
        assert!((1..NODES)
            .all(|index| authority.dependency_digest(&format!("cycle-{index:04}")) == expected));

        sources.reverse();
        let reversed = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
        assert_eq!(reversed.dependency_digest("cycle-0000"), expected);
        assert!(reversed.dependency_work_units <= NODES.saturating_mul(7));
    }

    #[test]
    fn poisoned_fact_lock_is_typed_internal_failure() {
        let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
        let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
        authority.poison_facts_lock();
        assert!(matches!(
            authority.classify("child", "call", "call", None, 0, 0),
            Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
        ));
    }

    #[test]
    fn fact_budget_exhaustion_is_typed() {
        let sources = vec![source("root", None, 1)];
        let authority = CodexOutcomeLineageAuthorityV0::from_sources_with_budget(
            &sources,
            Arc::new(CodexLineageFactBudgetV0::with_limits(1, 1)),
        )
        .unwrap();
        assert!(matches!(
            authority.new_fact_set(),
            Err(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)
        ));
    }

    #[test]
    fn cyclic_relationship_is_unproven_without_waiting_for_fact_registration() {
        let sources = vec![
            source("left", Some("right"), 1),
            source("right", Some("left"), 2),
        ];
        let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
        assert_eq!(
            authority
                .classify("left", "call", "call", Some(101), 102, 100)
                .unwrap(),
            CodexOutcomeOriginV0::Unproven
        );
    }

    #[test]
    fn only_exact_post_fork_correlated_results_bypass_ancestor_lookup() {
        assert!(exact_correlated_result_postdates_fork(
            "call-1",
            "call-1",
            Some(101),
            102,
            100,
        ));

        for candidate in [
            ("call-1", "call-2", Some(101), 102, 100),
            ("", "", Some(101), 102, 100),
            ("call-1", "call-1", None, 102, 100),
            ("call-1", "call-1", Some(100), 102, 100),
            ("call-1", "call-1", Some(99), 102, 100),
            ("call-1", "call-1", Some(101), 100, 100),
            ("call-1", "call-1", Some(101), 99, 100),
        ] {
            assert!(!exact_correlated_result_postdates_fork(
                candidate.0,
                candidate.1,
                candidate.2,
                candidate.3,
                candidate.4,
            ));
        }
    }

    #[test]
    fn parent_owned_by_another_route_is_conservatively_unproven() {
        let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
        let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
        authority
            .bind_route_sources(&HashSet::from(["child".to_owned()]))
            .unwrap();
        assert_eq!(
            authority
                .classify("child", "call", "call", None, 0, 0)
                .unwrap(),
            CodexOutcomeOriginV0::Unproven
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
            authority.classify("child", "call", "call", None, 0, 0),
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
            .register("child", authority.new_fact_set().unwrap())
            .unwrap();
        let facts = authority.facts.lock().unwrap();
        let child = authority.indices["child"];
        assert!(matches!(facts[child], LineageFactsStateV0::CompleteLeaf));
        drop(facts);
        assert!(matches!(
            authority.register("child", authority.new_fact_set().unwrap()),
            Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
        ));
    }
}

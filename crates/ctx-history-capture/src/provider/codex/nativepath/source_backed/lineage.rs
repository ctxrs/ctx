use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::*;
use crate::provider::codex::nativepath::reader::CodexLineageFactPresenceV0;

// V4 binds the complete normalized lineage tuple into warm replay.
const LINEAGE_DEPENDENCY_DOMAIN: &[u8] = b"ctx/codex-lineage-dependency/v4\0";
const MAX_CODEX_LINEAGE_NODES: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CodexLineageRejectionReasonV0 {
    DuplicateNativeSessionId,
    MissingParent { parent_native_session_id: String },
    SelfParent,
    Cycle { canonical_native_session_id: String },
    DepthExceeded,
    ContradictoryDirectParentEvidence,
    AdvisoryUnrelatedComponent { advisory_session_id: String },
    AdvisoryIrreconcilable { advisory_session_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct CodexLineageRejectionProofV0 {
    version: u8,
    native_session_id: String,
    component_native_session_id: String,
    evidence_native_session_id: String,
    reason: CodexLineageRejectionReasonV0,
}

#[derive(Debug, Clone)]
pub(super) struct CodexLineageRejectedSourceV0 {
    pub(super) source: CodexCatalogSource,
    pub(super) proof: CodexLineageRejectionProofV0,
}

pub(super) struct CodexLineageNormalizationV0 {
    pub(super) sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    pub(super) rejections: Vec<CodexLineageRejectedSourceV0>,
    pub(super) authority: CodexOutcomeLineageAuthorityV0,
}

#[derive(Debug, Clone)]
struct ComponentIssueV0 {
    evidence_native_session_id: String,
    reason: CodexLineageRejectionReasonV0,
}

struct DisjointComponentsV0 {
    parents: Vec<usize>,
    ranks: Vec<u8>,
}

impl DisjointComponentsV0 {
    fn new(len: usize) -> Self {
        Self {
            parents: (0..len).collect(),
            ranks: vec![0; len],
        }
    }

    fn find(&mut self, mut index: usize) -> usize {
        let mut root = index;
        while self.parents[root] != root {
            root = self.parents[root];
        }
        while self.parents[index] != index {
            let parent = self.parents[index];
            self.parents[index] = root;
            index = parent;
        }
        root
    }

    fn union(&mut self, left: usize, right: usize) {
        let mut left = self.find(left);
        let mut right = self.find(right);
        if left == right {
            return;
        }
        if self.ranks[left] < self.ranks[right] {
            std::mem::swap(&mut left, &mut right);
        }
        self.parents[right] = left;
        if self.ranks[left] == self.ranks[right] {
            self.ranks[left] = self.ranks[left].saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CodexOutcomeOriginV0 {
    UniqueToSession,
    CopiedFromAncestor { ancestor_native_session_id: String },
    Unproven,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParentLinkV0 {
    Root,
    Source(usize),
}

#[derive(Debug)]
struct LineageNodeV0 {
    native_session_id: String,
    observation: CodexFileObservation,
    parent: ParentLinkV0,
    relationship: SessionRelationshipKind,
    advisory_session_id: Option<String>,
    root_native_session_id: String,
    dependency_digest: [u8; 32],
    depth: usize,
    component_digest: [u8; 32],
    component: usize,
}

#[derive(Debug)]
enum LineageFactsStateV0 {
    Pending,
    OutsideRoute,
    CompleteLeaf,
    Ready(CodexLineageFactsV0),
    Released,
}

#[derive(Debug)]
pub(super) struct CodexOutcomeLineageAuthorityV0 {
    nodes: Vec<LineageNodeV0>,
    indices: HashMap<String, usize>,
    facts: Mutex<Vec<LineageFactsStateV0>>,
    needs_descendant_facts: Mutex<Vec<bool>>,
    component_budgets: Vec<Arc<CodexLineageFactBudgetV0>>,
    component_members: Vec<Box<[usize]>>,
    #[cfg(test)]
    dependency_work_units: usize,
}

impl CodexOutcomeLineageAuthorityV0 {
    pub(super) fn normalize_sources(
        sources: &[(CodexCatalogSource, SourceKey, String)],
    ) -> CodexSourceBackedResultV0<CodexLineageNormalizationV0> {
        Self::normalize_sources_with_optional_budget(sources, None)
    }

    #[cfg(test)]
    pub(super) fn normalize_sources_with_budget(
        sources: &[(CodexCatalogSource, SourceKey, String)],
        budget: Arc<CodexLineageFactBudgetV0>,
    ) -> CodexSourceBackedResultV0<CodexLineageNormalizationV0> {
        Self::normalize_sources_with_optional_budget(sources, Some(budget))
    }

    #[cfg(test)]
    pub(super) fn from_sources(
        sources: &[(CodexCatalogSource, SourceKey, String)],
    ) -> CodexSourceBackedResultV0<Self> {
        let normalized = Self::normalize_sources(sources)?;
        if !normalized.rejections.is_empty() {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex lineage source graph contains rejected components".to_owned(),
                ),
            ));
        }
        Ok(normalized.authority)
    }

    #[cfg(test)]
    pub(super) fn from_sources_with_budget(
        sources: &[(CodexCatalogSource, SourceKey, String)],
        budget: Arc<CodexLineageFactBudgetV0>,
    ) -> CodexSourceBackedResultV0<Self> {
        let normalized = Self::normalize_sources_with_budget(sources, budget)?;
        if !normalized.rejections.is_empty() {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::InvalidPayload(
                    "Codex lineage source graph contains rejected components".to_owned(),
                ),
            ));
        }
        Ok(normalized.authority)
    }

    fn normalize_sources_with_optional_budget(
        sources: &[(CodexCatalogSource, SourceKey, String)],
        budget_override: Option<Arc<CodexLineageFactBudgetV0>>,
    ) -> CodexSourceBackedResultV0<CodexLineageNormalizationV0> {
        let mut ordered = sources.to_vec();
        ordered.sort_by(|left, right| {
            left.2
                .cmp(&right.2)
                .then_with(|| left.0.source_path.cmp(&right.0.source_path))
        });
        let mut groups = BTreeMap::<String, Vec<usize>>::new();
        for (index, (_, _, native_session_id)) in ordered.iter().enumerate() {
            groups
                .entry(native_session_id.clone())
                .or_default()
                .push(index);
        }

        let mut components = DisjointComponentsV0::new(ordered.len());
        for members in groups.values() {
            if let Some((&first, rest)) = members.split_first() {
                for member in rest {
                    components.union(first, *member);
                }
            }
        }
        for (index, (source, _, _)) in ordered.iter().enumerate() {
            if let Some(parent) = source.catalog_parent_native_session_id.as_ref() {
                if let Some(parent_members) = groups.get(parent) {
                    for parent_index in parent_members {
                        components.union(index, *parent_index);
                    }
                }
            }
        }
        let component_of = (0..ordered.len())
            .map(|index| components.find(index))
            .collect::<Vec<_>>();
        let mut component_members = BTreeMap::<usize, Vec<usize>>::new();
        for (index, component) in component_of.iter().copied().enumerate() {
            component_members.entry(component).or_default().push(index);
        }
        let mut issues = HashMap::<usize, ComponentIssueV0>::new();
        macro_rules! reject {
            ($index:expr, $reason:expr $(,)?) => {{
                let index = $index;
                issues
                    .entry(component_of[index])
                    .or_insert_with(|| ComponentIssueV0 {
                        evidence_native_session_id: ordered[index].2.clone(),
                        reason: $reason,
                    });
            }};
        }

        for members in groups.values().filter(|members| members.len() > 1) {
            for member in members {
                reject!(
                    *member,
                    CodexLineageRejectionReasonV0::DuplicateNativeSessionId
                );
            }
        }

        let mut parent_indices = vec![None; ordered.len()];
        for (index, (source, _, native_session_id)) in ordered.iter().enumerate() {
            match (
                source.catalog_parent_native_session_id.as_ref(),
                source.catalog_session_relationship,
            ) {
                (None, SessionRelationshipKind::Root) => {}
                (Some(_), SessionRelationshipKind::Root)
                | (None, _)
                | (_, SessionRelationshipKind::RelatedUnknown) => reject!(
                    index,
                    CodexLineageRejectionReasonV0::ContradictoryDirectParentEvidence,
                ),
                (Some(_), _) => {}
            }
            let Some(parent) = source.catalog_parent_native_session_id.as_ref() else {
                continue;
            };
            if parent == native_session_id {
                reject!(index, CodexLineageRejectionReasonV0::SelfParent);
                continue;
            }
            match groups.get(parent).map(Vec::as_slice) {
                Some([parent_index]) => parent_indices[index] = Some(*parent_index),
                Some(_) => {}
                None => reject!(
                    index,
                    CodexLineageRejectionReasonV0::MissingParent {
                        parent_native_session_id: parent.clone(),
                    },
                ),
            }
        }

        let mut colors = vec![0_u8; ordered.len()];
        let mut roots = vec![None; ordered.len()];
        let mut depths = vec![0_usize; ordered.len()];
        for start in 0..ordered.len() {
            let component = component_of[start];
            if colors[start] == 2 || issues.contains_key(&component) {
                continue;
            }
            let mut path = Vec::new();
            let mut current = start;
            loop {
                match colors[current] {
                    0 => {
                        if path.len() == MAX_CODEX_LINEAGE_NODES {
                            reject!(start, CodexLineageRejectionReasonV0::DepthExceeded);
                            break;
                        }
                        colors[current] = 1;
                        path.push(current);
                        match parent_indices[current] {
                            Some(parent) => current = parent,
                            None => {
                                roots[current] = Some(current);
                                depths[current] = 0;
                                colors[current] = 2;
                                break;
                            }
                        }
                    }
                    1 => {
                        let cycle_start =
                            path.iter()
                                .position(|candidate| *candidate == current)
                                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
                        let canonical = path[cycle_start..]
                            .iter()
                            .map(|index| ordered[*index].2.as_str())
                            .min()
                            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
                            .to_owned();
                        reject!(
                            start,
                            CodexLineageRejectionReasonV0::Cycle {
                                canonical_native_session_id: canonical,
                            },
                        );
                        break;
                    }
                    2 => break,
                    _ => return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable),
                }
            }
            if issues.contains_key(&component) {
                for index in path {
                    colors[index] = 2;
                }
                continue;
            }
            for index in path.into_iter().rev() {
                if colors[index] == 2 {
                    continue;
                }
                let parent = parent_indices[index]
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
                let root =
                    roots[parent].ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
                let depth = depths[parent].saturating_add(1);
                if depth >= MAX_CODEX_LINEAGE_NODES {
                    reject!(index, CodexLineageRejectionReasonV0::DepthExceeded);
                    break;
                }
                roots[index] = Some(root);
                depths[index] = depth;
                colors[index] = 2;
            }
            if issues.contains_key(&component) {
                for member in &component_members[&component] {
                    colors[*member] = 2;
                }
            }
        }

        for (index, (source, _, _)) in ordered.iter().enumerate() {
            let component = component_of[index];
            if issues.contains_key(&component) {
                continue;
            }
            let Some(advisory) = source.catalog_advisory_session_id.as_ref() else {
                continue;
            };
            let root_index =
                roots[index].ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            if advisory == &ordered[root_index].2 || advisory == &ordered[index].2 {
                continue;
            }
            let advisory_index = match groups.get(advisory).map(Vec::as_slice) {
                Some([advisory_index]) => *advisory_index,
                Some(_) | None => {
                    reject!(
                        index,
                        CodexLineageRejectionReasonV0::AdvisoryIrreconcilable {
                            advisory_session_id: advisory.clone(),
                        },
                    );
                    continue;
                }
            };
            if component_of[advisory_index] != component {
                reject!(
                    index,
                    CodexLineageRejectionReasonV0::AdvisoryUnrelatedComponent {
                        advisory_session_id: advisory.clone(),
                    },
                );
                continue;
            }
            let mut ancestor = parent_indices[index];
            let mut corroborated = false;
            for _ in 0..MAX_CODEX_LINEAGE_NODES {
                let Some(candidate) = ancestor else {
                    break;
                };
                if candidate == advisory_index {
                    corroborated = true;
                    break;
                }
                ancestor = parent_indices[candidate];
            }
            if !corroborated {
                reject!(
                    index,
                    CodexLineageRejectionReasonV0::AdvisoryIrreconcilable {
                        advisory_session_id: advisory.clone(),
                    },
                );
            }
        }

        let component_native_session_ids = component_members
            .iter()
            .map(|(component, members)| {
                members
                    .first()
                    .map(|index| (*component, ordered[*index].2.clone()))
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
            })
            .collect::<CodexSourceBackedResultV0<HashMap<_, _>>>()?;
        let normalized_root_ids = roots
            .iter()
            .map(|root| root.map(|index| ordered[index].2.clone()))
            .collect::<Vec<_>>();
        let mut normalized_sources = Vec::new();
        let mut normalized_depths = Vec::new();
        let mut rejections = Vec::new();
        for (index, mut plan) in ordered.into_iter().enumerate() {
            let component = component_of[index];
            if let Some(issue) = issues.get(&component) {
                let component_native_session_id = component_native_session_ids
                    .get(&component)
                    .cloned()
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
                rejections.push(CodexLineageRejectedSourceV0 {
                    source: plan.0,
                    proof: CodexLineageRejectionProofV0 {
                        version: 1,
                        native_session_id: plan.2,
                        component_native_session_id,
                        evidence_native_session_id: issue.evidence_native_session_id.clone(),
                        reason: issue.reason.clone(),
                    },
                });
                continue;
            }
            plan.0.catalog_root_native_session_id = Some(
                normalized_root_ids[index]
                    .clone()
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?,
            );
            normalized_depths.push(depths[index]);
            normalized_sources.push(plan);
        }
        let authority = Self::from_normalized_sources_with_optional_budget(
            &normalized_sources,
            &normalized_depths,
            budget_override,
        )?;
        Ok(CodexLineageNormalizationV0 {
            sources: normalized_sources,
            rejections,
            authority,
        })
    }

    fn from_normalized_sources_with_optional_budget(
        sources: &[(CodexCatalogSource, SourceKey, String)],
        depths: &[usize],
        budget_override: Option<Arc<CodexLineageFactBudgetV0>>,
    ) -> CodexSourceBackedResultV0<Self> {
        if sources.len() != depths.len() {
            return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
        }
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
        for ((source, _, native_session_id), depth) in sources.iter().zip(depths) {
            let parent = match source.catalog_parent_native_session_id.as_ref() {
                None => ParentLinkV0::Root,
                Some(parent) => indices
                    .get(parent)
                    .copied()
                    .map(ParentLinkV0::Source)
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?,
            };
            nodes.push(LineageNodeV0 {
                native_session_id: native_session_id.clone(),
                observation: source.catalog_observation.clone(),
                parent,
                relationship: source.catalog_session_relationship,
                advisory_session_id: source.catalog_advisory_session_id.clone(),
                root_native_session_id: source
                    .catalog_root_native_session_id
                    .clone()
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?,
                dependency_digest: [0; 32],
                depth: *depth,
                component_digest: [0; 32],
                component: 0,
            });
        }
        // Direct/native scanner callers do not bind a route selection. Keep
        // their historical all-source behavior as the initial policy; the
        // shared family replaces this with the narrower route-local set before
        // any leaf workers start.
        let mut needs_descendant_facts = vec![false; nodes.len()];
        for node in &nodes {
            if let ParentLinkV0::Source(parent) = node.parent {
                *needs_descendant_facts
                    .get_mut(parent)
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? = true;
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
        let mut component_digests = nodes
            .iter()
            .map(|node| node.component_digest)
            .collect::<Vec<_>>();
        component_digests.sort_unstable();
        component_digests.dedup();
        for node in &mut nodes {
            node.component = component_digests
                .binary_search(&node.component_digest)
                .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        }
        let mut component_members = (0..component_digests.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        for (index, node) in nodes.iter().enumerate() {
            component_members
                .get_mut(node.component)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
                .push(index);
        }
        let component_members = component_members
            .into_iter()
            .map(Vec::into_boxed_slice)
            .collect();
        let component_budgets = (0..component_digests.len())
            .map(|_| {
                budget_override
                    .as_ref()
                    .map_or_else(|| Arc::new(CodexLineageFactBudgetV0::default()), Arc::clone)
            })
            .collect();
        Ok(Self {
            nodes,
            indices,
            facts: Mutex::new(facts),
            needs_descendant_facts: Mutex::new(needs_descendant_facts),
            component_budgets,
            component_members,
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
            needs_descendant_facts: Mutex::new(Vec::new()),
            component_budgets: Vec::new(),
            component_members: Vec::new(),
            dependency_work_units: 0,
        }
    }

    pub(super) fn new_fact_set(
        &self,
        native_session_id: &str,
    ) -> CodexSourceBackedResultV0<CodexLineageFactsV0> {
        let component = self
            .indices
            .get(native_session_id)
            .and_then(|index| self.nodes.get(*index))
            .map(|node| node.component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let budget = self
            .component_budgets
            .get(component)
            .cloned()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        CodexLineageFactsV0::new(budget).map_err(map_lineage_capture_error)
    }

    pub(super) fn bind_route_sources(
        &self,
        selected_native_session_ids: &HashSet<String>,
    ) -> CodexSourceBackedResultV0<()> {
        let mut needs_descendant_facts = vec![false; self.nodes.len()];
        for node in &self.nodes {
            if !selected_native_session_ids.contains(&node.native_session_id) {
                continue;
            }
            if let ParentLinkV0::Source(parent) = node.parent {
                if self.nodes.get(parent).is_some_and(|parent| {
                    selected_native_session_ids.contains(&parent.native_session_id)
                }) {
                    *needs_descendant_facts
                        .get_mut(parent)
                        .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? = true;
                }
            }
        }
        *self
            .needs_descendant_facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? =
            needs_descendant_facts;
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
        let retain_facts = *self
            .needs_descendant_facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
            .get(index)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
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
            | LineageFactsStateV0::Ready(_)
            | LineageFactsStateV0::Released => {
                Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
            }
        }
    }

    pub(super) fn component_partition(&self, native_session_id: &str) -> Option<u64> {
        self.indices
            .get(native_session_id)
            .and_then(|index| self.nodes.get(*index))
            .and_then(|node| u64::try_from(node.component).ok())
    }

    pub(super) fn needs_descendant_facts(
        &self,
        native_session_id: &str,
    ) -> CodexSourceBackedResultV0<bool> {
        let index = self
            .indices
            .get(native_session_id)
            .copied()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        self.needs_descendant_facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
            .get(index)
            .copied()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    }

    pub(super) fn release_component(&self, component: u64) -> CodexSourceBackedResultV0<()> {
        let component = usize::try_from(component)
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let members = self
            .component_members
            .get(component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        for index in members {
            let state = facts
                .get_mut(*index)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            if !matches!(state, LineageFactsStateV0::OutsideRoute) {
                *state = LineageFactsStateV0::Released;
            }
        }
        Ok(())
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

    pub(super) fn classify(
        &self,
        native_session_id: &str,
        origin_call_id: &str,
        result_call_id: &str,
    ) -> CodexSourceBackedResultV0<CodexOutcomeOriginV0> {
        let Some(current) = self
            .indices
            .get(native_session_id)
            .and_then(|index| self.nodes.get(*index))
        else {
            return Ok(CodexOutcomeOriginV0::Unproven);
        };
        // Codex event timestamps are not lineage authority: copied native rows
        // may be reordered or restamped. Only an exhaustive walk of certified
        // ancestor call/result facts can prove copied presence or unique absence.
        let mut parent = match &current.parent {
            ParentLinkV0::Root => return Ok(CodexOutcomeOriginV0::UniqueToSession),
            ParentLinkV0::Source(index) => ParentLinkV0::Source(*index),
        };
        let facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut remaining = self.nodes.len().saturating_add(1);
        while remaining != 0 {
            remaining = remaining.saturating_sub(1);
            let parent_index = match parent {
                ParentLinkV0::Root => return Ok(CodexOutcomeOriginV0::UniqueToSession),
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
                Some(LineageFactsStateV0::Pending | LineageFactsStateV0::Released) | None => {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
                }
            };
            match parent_facts.presence(origin_call_id, result_call_id) {
                CodexLineageFactPresenceV0::Present => {
                    return Ok(CodexOutcomeOriginV0::CopiedFromAncestor {
                        ancestor_native_session_id: parent_node.native_session_id.clone(),
                    })
                }
                CodexLineageFactPresenceV0::Unproven => return Ok(CodexOutcomeOriginV0::Unproven),
                CodexLineageFactPresenceV0::Absent => {}
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

fn compute_dependency_digests(nodes: &mut [LineageNodeV0]) -> CodexSourceBackedResultV0<usize> {
    let mut order = (0..nodes.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        nodes[*left].depth.cmp(&nodes[*right].depth).then_with(|| {
            nodes[*left]
                .native_session_id
                .cmp(&nodes[*right].native_session_id)
        })
    });
    let mut work_units = 0_usize;
    for index in order {
        let mut hasher = dependency_hasher(b"normalized-node\0");
        hash_text(&mut hasher, &nodes[index].native_session_id);
        match &nodes[index].parent {
            ParentLinkV0::Root => hasher.update([0]),
            ParentLinkV0::Source(parent) => {
                hasher.update([1]);
                hash_text(&mut hasher, &nodes[*parent].native_session_id);
            }
        }
        hash_text(&mut hasher, nodes[index].relationship.as_str());
        hash_optional_text(&mut hasher, nodes[index].advisory_session_id.as_deref());
        hash_text(&mut hasher, &nodes[index].root_native_session_id);
        match nodes[index].parent {
            ParentLinkV0::Root => {
                if nodes[index].depth != 0
                    || nodes[index].relationship != SessionRelationshipKind::Root
                    || nodes[index].root_native_session_id != nodes[index].native_session_id
                {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
                }
                let mut component = dependency_hasher(b"normalized-component\0");
                hash_text(&mut component, &nodes[index].root_native_session_id);
                nodes[index].component_digest = component.finalize().into();
            }
            ParentLinkV0::Source(parent) => {
                if nodes[index].depth != nodes[parent].depth.saturating_add(1)
                    || nodes[index].root_native_session_id != nodes[parent].root_native_session_id
                    || nodes[index].relationship == SessionRelationshipKind::Root
                    || nodes[index].relationship == SessionRelationshipKind::RelatedUnknown
                {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
                }
                hash_observation(&mut hasher, &nodes[parent].observation);
                hasher.update(nodes[parent].dependency_digest);
                nodes[index].component_digest = nodes[parent].component_digest;
            }
        }
        nodes[index].dependency_digest = hasher.finalize().into();
        work_units = work_units.saturating_add(1);
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

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_text(hasher, value);
        }
        None => hasher.update([0]),
    }
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
        let reversed =
            CodexOutcomeLineageAuthorityV0::normalize_sources(&reversed_sources).unwrap();
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
    fn parent_owned_by_another_route_is_conservatively_unproven() {
        let sources = vec![source("root", None, 1), source("child", Some("root"), 2)];
        let authority = CodexOutcomeLineageAuthorityV0::from_sources(&sources).unwrap();
        authority
            .bind_route_sources(&HashSet::from(["child".to_owned()]))
            .unwrap();
        assert_eq!(
            authority.classify("child", "call", "call").unwrap(),
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
}

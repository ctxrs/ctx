use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    sync::{Arc, Mutex},
};

use serde::Serialize;

use super::*;
use crate::provider::codex::nativepath::reader::CodexLineageFactPresenceV0;

mod dependency;
mod generation_cache;
#[cfg(test)]
mod tests;

use dependency::{compute_dependency_digests, digest_marker};
use generation_cache::GenerationLineageCacheV0;

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
    generation_spill: Option<Mutex<File>>,
    generation_spill_entries: Vec<Option<CodexLineageFactsSpillRecordV0>>,
    generation_cache: Option<Mutex<GenerationLineageCacheV0>>,
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
        let generation_spill_entries = vec![None; nodes.len()];
        Ok(Self {
            nodes,
            indices,
            facts: Mutex::new(facts),
            needs_descendant_facts: Mutex::new(needs_descendant_facts),
            component_budgets,
            component_members,
            generation_spill: None,
            generation_spill_entries,
            generation_cache: None,
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
            generation_spill: None,
            generation_spill_entries: Vec::new(),
            generation_cache: None,
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
        let mut participates = vec![false; self.nodes.len()];
        for node in &self.nodes {
            if !selected_native_session_ids.contains(&node.native_session_id) {
                continue;
            }
            let mut current = *self
                .indices
                .get(&node.native_session_id)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            *participates
                .get_mut(current)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? = true;
            let mut remaining = self.nodes.len().saturating_add(1);
            while remaining != 0 {
                remaining = remaining.saturating_sub(1);
                let Some(parent) = self.nodes.get(current).and_then(|node| match node.parent {
                    ParentLinkV0::Root => None,
                    ParentLinkV0::Source(parent) => Some(parent),
                }) else {
                    break;
                };
                *participates
                    .get_mut(parent)
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? = true;
                *needs_descendant_facts
                    .get_mut(parent)
                    .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? = true;
                current = parent;
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
        for (participates, state) in participates.into_iter().zip(facts.iter_mut()) {
            if !participates {
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

    pub(super) fn register_certified(
        &self,
        native_session_id: &str,
        authority: &CodexCertifiedLineageFactsV0,
    ) -> CodexSourceBackedResultV0<()> {
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
        let facts = CodexLineageFactsV0::from_certified_authority(authority, budget)
            .map_err(map_lineage_capture_error)?;
        self.register(native_session_id, facts)
    }

    pub(super) fn certified_authority(
        &self,
        native_session_id: &str,
    ) -> CodexSourceBackedResultV0<Option<CodexCertifiedLineageFactsV0>> {
        let index = self
            .indices
            .get(native_session_id)
            .copied()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        match facts.get(index) {
            Some(LineageFactsStateV0::Ready(facts)) => Ok(facts.certified_authority()),
            Some(LineageFactsStateV0::CompleteLeaf) => Ok(None),
            _ => Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable),
        }
    }

    pub(super) fn generation_fact_state_ready(
        &self,
        native_session_id: &str,
    ) -> CodexSourceBackedResultV0<bool> {
        let index = self
            .indices
            .get(native_session_id)
            .copied()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        Ok(matches!(
            self.facts
                .lock()
                .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
                .get(index),
            Some(LineageFactsStateV0::Ready(_) | LineageFactsStateV0::CompleteLeaf)
        ))
    }

    pub(super) fn generation_participates(
        &self,
        native_session_id: &str,
    ) -> CodexSourceBackedResultV0<bool> {
        let index = self
            .indices
            .get(native_session_id)
            .copied()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        Ok(!matches!(
            self.facts
                .lock()
                .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?
                .get(index),
            Some(LineageFactsStateV0::OutsideRoute)
        ))
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

pub(super) fn map_lineage_capture_error(error: CaptureError) -> CodexSourceBackedErrorV0 {
    match &error {
        CaptureError::InvalidPayload(detail) if detail == CODEX_LINEAGE_EXHAUSTED_SENTINEL => {
            CodexSourceBackedErrorV0::LineageWorkingSetExhausted
        }
        _ => CodexSourceBackedErrorV0::Capture(error),
    }
}

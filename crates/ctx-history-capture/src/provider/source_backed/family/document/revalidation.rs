use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Mutex,
};

use ctx_history_core::{CertifiedSource, CertifiedSourceInventory, SourceKey};

use super::{CompleteDocumentTree, ReplacementDocumentTree};
use crate::provider::source_backed::SourceBackedRevalidationTarget;

pub(super) struct CurrentDocumentSources {
    ordered: Vec<SourceKey>,
    canonical: HashMap<[u8; 32], usize>,
    exact_descriptors: HashMap<[u8; 32], ExactDescriptorEntry>,
    #[cfg(test)]
    operations: DocumentMembershipOperations,
}

struct ExactDescriptorEntry {
    first: usize,
    collisions: Vec<usize>,
}

impl CurrentDocumentSources {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            ordered: Vec::with_capacity(capacity),
            canonical: HashMap::with_capacity(capacity),
            exact_descriptors: HashMap::with_capacity(capacity),
            #[cfg(test)]
            operations: DocumentMembershipOperations::default(),
        }
    }

    pub(super) fn contains_canonical(&mut self, source: &SourceKey) -> bool {
        self.canonical_source(source).is_some()
    }

    pub(super) fn canonical_source(&mut self, source: &SourceKey) -> Option<&SourceKey> {
        #[cfg(test)]
        {
            self.operations.canonical_lookups += 1;
        }
        self.canonical
            .get(&source.identity().digest())
            .and_then(|index| self.ordered.get(*index))
    }

    pub(super) fn insert(&mut self, source: SourceKey) -> bool {
        #[cfg(test)]
        {
            self.operations.source_insertions += 1;
        }
        let canonical_digest = source.identity().digest();
        let Entry::Vacant(canonical) = self.canonical.entry(canonical_digest) else {
            return false;
        };
        let descriptor_digest = source.exact_descriptor_digest();
        let index = self.ordered.len();
        canonical.insert(index);
        self.ordered.push(source);
        self.exact_descriptors
            .entry(descriptor_digest)
            .and_modify(|entry| entry.collisions.push(index))
            .or_insert_with(|| ExactDescriptorEntry {
                first: index,
                collisions: Vec::new(),
            });
        true
    }

    pub(super) fn contains_exact(&mut self, source: &SourceKey) -> bool {
        #[cfg(test)]
        {
            self.operations.exact_lookups += 1;
        }
        let Some(entry) = self
            .exact_descriptors
            .get(&source.exact_descriptor_digest())
        else {
            return false;
        };
        let mut comparisons = 0;
        let matched = std::iter::once(&entry.first)
            .chain(&entry.collisions)
            .any(|index| {
                comparisons += 1;
                self.ordered
                    .get(*index)
                    .is_some_and(|candidate| candidate.exact_descriptor_eq(source))
            });
        #[cfg(test)]
        {
            self.operations.exact_comparisons += comparisons;
        }
        #[cfg(not(test))]
        let _ = comparisons;
        matched
    }

    pub(super) fn ordered_inventory_sources(&self) -> Vec<SourceKey> {
        self.ordered.clone()
    }

    #[cfg(test)]
    pub(super) fn reset_operations(&mut self) {
        self.operations = DocumentMembershipOperations::default();
    }

    #[cfg(test)]
    pub(super) fn operations(&self) -> DocumentMembershipOperations {
        self.operations
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DocumentMembershipOperations {
    pub(super) source_insertions: usize,
    pub(super) canonical_lookups: usize,
    pub(super) exact_lookups: usize,
    pub(super) exact_comparisons: usize,
}

pub(super) struct DocumentCommitState<L, A> {
    pub(super) expected: Option<ExpectedDocumentRoute<L, A>>,
}

impl<L, A> Default for DocumentCommitState<L, A> {
    fn default() -> Self {
        Self { expected: None }
    }
}

pub(super) struct ExpectedDocumentRoute<L, A> {
    pub(super) tree: CompleteDocumentTree<L, A>,
    pub(super) certificates: HashMap<[u8; 32], CertifiedSource>,
    inventory: CertifiedSourceInventory,
}

impl<L, A> ExpectedDocumentRoute<L, A> {
    pub(super) fn new(
        tree: CompleteDocumentTree<L, A>,
        certificates: Vec<CertifiedSource>,
        inventory: CertifiedSourceInventory,
    ) -> Self {
        Self {
            tree,
            certificates: certificates
                .into_iter()
                .map(|certificate| {
                    (
                        certificate.observation().source().identity().digest(),
                        certificate,
                    )
                })
                .collect(),
            inventory,
        }
    }
}

pub(super) fn revalidate_document_target<L, A>(
    state: &Mutex<DocumentCommitState<L, A>>,
    target: SourceBackedRevalidationTarget<'_>,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    state
        .expected
        .as_ref()
        .is_some_and(|expected| match target {
            SourceBackedRevalidationTarget::Source(source) => {
                expected
                    .certificates
                    .get(&source.observation().source().identity().digest())
                    == Some(source)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                deletion.verifies(&expected.inventory)
                    && !expected
                        .certificates
                        .contains_key(&deletion.source().identity().digest())
            }
        })
}

pub(super) fn revalidate_document_inventory<A>(
    adapter: &A,
    state: &Mutex<DocumentCommitState<A::Leaf, A::TreeAuthority>>,
    inventory: &CertifiedSourceInventory,
) -> bool
where
    A: ReplacementDocumentTree,
{
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(expected) = state.expected.as_ref() else {
        return false;
    };
    if expected.inventory != *inventory {
        return false;
    }

    // Source and deletion callbacks only bind writer targets to this expected
    // route. The final inventory callback owns the one live terminal tree
    // observation, so changes between callbacks cannot inherit an earlier
    // successful result.
    adapter
        .revalidate_complete(&expected.tree)
        .is_ok_and(|terminal| terminal == expected.tree.tree_fingerprint)
        && revalidate_durable_replay_sources(adapter, &expected.tree)
}

fn revalidate_durable_replay_sources<A>(
    adapter: &A,
    tree: &CompleteDocumentTree<A::Leaf, A::TreeAuthority>,
) -> bool
where
    A: ReplacementDocumentTree,
{
    tree.leaves.iter().all(|observed| {
        if !observed.replay_from_frontier {
            return true;
        }
        adapter
            .durable_replay_source(&tree.authority, &observed.provider_leaf)
            .is_ok_and(|current| match (&observed.bound_replay_source, current) {
                (Some(expected), Some(current)) => expected.exact_descriptor_eq(&current),
                (None, None) => true,
                _ => false,
            })
    })
}

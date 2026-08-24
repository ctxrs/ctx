use super::*;

fn certified_owned_same_path_replacement<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &JsonlFamilyInventory<JsonlRuntimeError<R>>,
    inventory: &CertifiedSourceInventory,
    terminal_sources: &HashMap<[u8; 32], TerminalSourceEvidence<JsonlRuntimeError<R>>>,
    base_source: &SourceKey,
    base_path: &Path,
) -> bool {
    adapter.owns(base_source)
        && opening.accepted_leaves().any(|leaf| {
            let replacement_source = leaf.source();
            leaf.source_path() == base_path
                && adapter.owns(replacement_source)
                && !replacement_source.exact_descriptor_eq(base_source)
                && inventory.contains(replacement_source)
                && terminal_sources
                    .get(&replacement_source.exact_descriptor_digest())
                    .is_some_and(|evidence| {
                        let evidence_source =
                            evidence.observed_certificate().observation().source();
                        adapter.owns(evidence_source)
                            && evidence_source.exact_descriptor_eq(replacement_source)
                    })
        })
}

pub(super) fn retirement_absence_dependency<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &JsonlFamilyInventory<JsonlRuntimeError<R>>,
    inventory: &CertifiedSourceInventory,
    terminal_sources: &HashMap<[u8; 32], TerminalSourceEvidence<JsonlRuntimeError<R>>>,
    base_source: &SourceKey,
    base_path: &Path,
) -> Option<JsonlFamilyAbsentMember<JsonlRuntimeError<R>>> {
    if certified_owned_same_path_replacement(
        adapter,
        opening,
        inventory,
        terminal_sources,
        base_source,
        base_path,
    ) {
        None
    } else {
        JsonlFamilyAbsentMember::from_path(opening, base_path.to_path_buf())
    }
}

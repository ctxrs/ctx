use super::*;

pub(super) fn can_reuse_admission<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    base: &CertifiedSource,
) -> bool {
    leaf.observation().supports_exact_revalidation()
        && decode_checkpoint(adapter, leaf, base).is_ok_and(|checkpoint| {
            checkpoint.physical.source_observation() == leaf.observation()
                && checkpoint.physical.logical_eof() == leaf.logical_eof()
                && checkpoint.exact_terminal_binding_matches(leaf)
                && (checkpoint.physical.terminal() || checkpoint.authenticates_admitted_eof())
        })
}

pub(super) fn validate_changed_leaves<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &mut JsonlFamilyInventory<JsonlRuntimeError<R>>,
    bases: &HashMap<[u8; 32], &CertifiedSource>,
) -> JsonlResult<(), JsonlRuntimeError<R>> {
    let mut changed = false;
    for member in &mut opening.members {
        let JsonlFamilyInventoryMember::Accepted { identity, leaf } = member else {
            continue;
        };
        if base_for_leaf(bases, leaf).is_some_and(|base| can_reuse_admission(adapter, leaf, base)) {
            continue;
        }
        if let Some(rejected) = adapter.validate_changed_leaf(leaf)? {
            if rejected.source_path != leaf.source_path
                || rejected.authority_path != leaf.authority_path
            {
                return Err(JsonlRuntimeError::<R>::system_invariant(
                    "JSONL admission rejection changed the physical member",
                ));
            }
            *member = JsonlFamilyInventoryMember::Quarantined {
                identity: *identity,
                leaf: rejected,
            };
            changed = true;
        }
    }
    if changed {
        opening.rebuild_observation()?;
    }
    Ok(())
}

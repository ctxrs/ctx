use super::*;

pub(super) fn can_reuse_admission<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    base: &CertifiedSource,
    append_only_trust_allowed: bool,
) -> bool {
    leaf.observation().supports_exact_revalidation()
        && decode_checkpoint(adapter, leaf, base).is_ok_and(|checkpoint| {
            let retained = checkpoint.physical.source_observation();
            // Direct append restores the admitted owner and checks new owner
            // records in semantic preflight. Re-reading old ownership rows
            // adds no evidence under the same-object append trust contract.
            checkpoint.physical.logical_eof() == leaf.logical_eof()
                && checkpoint.exact_terminal_binding_matches(leaf)
                && ((retained == leaf.observation()
                    && (checkpoint.physical.terminal() || checkpoint.authenticates_admitted_eof()))
                    || (append_only_trust_allowed
                        && adapter.append_trust_contract()
                            == super::super::JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
                        && adapter.allows_direct_append_for_leaf(leaf)
                        && leaf.observation().length() > retained.length()
                        && retained.admits_frozen_prefix_in(leaf.observation())
                        && checkpoint
                            .provider_checkpoint
                            .as_ref()
                            .is_some_and(|checkpoint| {
                                adapter.accepts_direct_append_checkpoint(checkpoint)
                            })))
        })
}

pub(super) fn validate_changed_leaves<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &mut JsonlFamilyInventory<JsonlRuntimeError<R>>,
    bases: &HashMap<[u8; 32], &CertifiedSource>,
    append_only_trust_allowed: bool,
) -> JsonlResult<(), JsonlRuntimeError<R>> {
    let mut changed = false;
    for member in &mut opening.members {
        let JsonlFamilyInventoryMember::Accepted { identity, leaf } = member else {
            continue;
        };
        if base_for_leaf(bases, leaf)
            .is_some_and(|base| can_reuse_admission(adapter, leaf, base, append_only_trust_allowed))
        {
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

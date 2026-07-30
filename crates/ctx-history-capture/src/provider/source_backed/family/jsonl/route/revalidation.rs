use super::*;

pub(super) fn reset_terminal(resident: &Mutex<FamilyResident>) -> SourceBackedRouteResult<()> {
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.terminal_sources.clear();
    resident.certified_inventory = None;
    Ok(())
}

pub(super) fn revalidate_target(
    resident: &Mutex<FamilyResident>,
    target: SourceBackedRevalidationTarget<'_>,
) -> bool {
    let Ok(resident) = resident.lock() else {
        return false;
    };
    match target {
        SourceBackedRevalidationTarget::Source(expected) => {
            let Some(evidence) = resident
                .terminal_sources
                .get(&expected.observation().source().exact_descriptor_digest())
            else {
                return false;
            };
            evidence.certificate == *expected
        }
        SourceBackedRevalidationTarget::Deletion(deletion) => resident
            .certified_inventory
            .as_ref()
            .is_some_and(|inventory| {
                deletion.verifies(inventory)
                    && !resident
                        .terminal_sources
                        .contains_key(&deletion.source().exact_descriptor_digest())
            }),
    }
}

pub(super) fn revalidate_complete_inventory(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    expected_inventory: &CertifiedSourceInventory,
) -> Result<bool> {
    let (expected_sources, certified_inventory) = {
        let resident = resident.lock().map_err(|_| {
            CaptureError::InvalidPayload("JSONL resident catalog lock was poisoned".to_owned())
        })?;
        (
            resident.terminal_sources.clone(),
            resident.certified_inventory.clone(),
        )
    };
    if certified_inventory.as_ref() != Some(expected_inventory) {
        return Ok(false);
    }

    // This is the single terminal filesystem witness for the route. Earlier
    // callbacks only bind writer targets; this callback rediscovers membership
    // and verifies every admitted leaf. Framed JSONL may grow only when its
    // exact certified prefix remains unchanged.
    let current = discover(adapter, root)?;
    if current.root_missing() || current.leaves().len() != expected_sources.len() {
        return Ok(false);
    }
    let current_inventory = current.certify_against(&current)?;
    if current_inventory != *expected_inventory {
        return Ok(false);
    }
    for leaf in current.leaves() {
        let Some(evidence) = expected_sources.get(&leaf.source().exact_descriptor_digest()) else {
            return Ok(false);
        };
        if !leaf
            .source()
            .exact_descriptor_eq(evidence.certificate.observation().source())
        {
            return Ok(false);
        }
        if leaf.whole_record {
            if source_observation(leaf.source(), leaf.observation())?
                != *evidence.certificate.observation()
            {
                return Ok(false);
            }
            drop(leaf.open_verified()?);
        } else if let Some(checkpoint) = evidence.checkpoint.as_ref() {
            let (opened, _) = leaf.open_for_hydration()?;
            revalidate_frozen_prefix(
                leaf.source_path(),
                opened.as_ref(),
                checkpoint.source_observation(),
                checkpoint.complete_prefix_end(),
                *checkpoint.complete_prefix_sha256(),
            )?;
        } else if source_observation(leaf.source(), leaf.observation())?
            != *evidence.certificate.observation()
        {
            return Ok(false);
        }
    }
    current.revalidate_root()?;
    Ok(true)
}

pub(super) fn inventory_observation(
    provider: CaptureProvider,
    root: &Path,
    missing: bool,
    authority: Option<&ProviderSourceRoot>,
    leaves: &[JsonlFamilyLeaf],
    rejected_leaves: &[JsonlFamilyRejectedLeaf],
) -> Result<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(FAMILY_INVENTORY_DOMAIN);
    digest.update([u8::from(missing)]);
    digest.update((leaves.len() as u64).to_be_bytes());
    digest.update((rejected_leaves.len() as u64).to_be_bytes());
    if let Some(authority) = authority {
        digest.update(authority.authority_fingerprint());
    }
    for leaf in leaves {
        digest.update([0]);
        digest.update(leaf.source.exact_descriptor_digest());
        digest.update([u8::from(leaf.whole_record)]);
        digest.update(
            (leaf.authority_path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes(),
        );
        digest.update(leaf.authority_path.as_os_str().as_encoded_bytes());
        digest.update(binding_digest(leaf)?);
    }
    for leaf in rejected_leaves {
        digest.update([1]);
        digest.update(
            (leaf.authority_path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes(),
        );
        digest.update(leaf.authority_path.as_os_str().as_encoded_bytes());
        digest.update((leaf.source_path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
        digest.update(leaf.source_path.as_os_str().as_encoded_bytes());
        digest.update(serde_json::to_vec(&leaf.proof)?);
    }
    SourceInventoryObservation::new(
        provider.as_str(),
        FAMILY_INVENTORY_AUTHORITY,
        TypedKey::bytes(root.as_os_str().as_encoded_bytes().to_vec()).map_err(contract_error)?,
        FAMILY_INVENTORY_REVISION,
        digest.finalize().to_vec(),
    )
    .map_err(contract_error)
}

pub(super) fn binding_digest(leaf: &JsonlFamilyLeaf) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(leaf.binding())?).into())
}

use super::*;

pub(super) fn reset_terminal(resident: &Mutex<FamilyResident>) -> SourceBackedRouteResult<()> {
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.terminal_sources.clear();
    resident.certified_inventory = None;
    resident.opening_inventory = None;
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
    let (owned_sources, expected_sources, certified_inventory, opening_inventory) = {
        let resident = resident.lock().map_err(|_| {
            CaptureError::InvalidPayload("JSONL resident catalog lock was poisoned".to_owned())
        })?;
        (
            resident.owned_sources.clone(),
            resident.terminal_sources.clone(),
            resident.certified_inventory.clone(),
            resident.opening_inventory.clone(),
        )
    };
    if certified_inventory.as_ref() != Some(expected_inventory) {
        return Ok(false);
    }
    let Some(opening_inventory) = opening_inventory else {
        return Ok(false);
    };
    if adapter.inventory_mode() == JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions {
        opening_inventory.revalidate_root_same_object()?;
    }

    // This is the single terminal filesystem witness for the route. Earlier
    // callbacks only bind writer targets; this callback rediscovers membership
    // and verifies every admitted leaf. Framed JSONL may grow only when its
    // exact certified prefix remains unchanged.
    let current = adapter.discover(root)?;
    if current.root_missing()
        && adapter.root_missing_mode() == JsonlFamilyRootMissingMode::Unavailable
    {
        return Ok(false);
    }
    if !current.retains_authorities_from(&opening_inventory) {
        return Ok(false);
    }
    let mut current_by_descriptor = HashMap::with_capacity(current.leaves().len());
    for leaf in current.leaves() {
        if current_by_descriptor
            .insert(leaf.source().exact_descriptor_digest(), leaf)
            .is_some()
        {
            return Ok(false);
        }
    }
    if adapter.inventory_mode() == JsonlFamilyInventoryMode::Exact {
        let current_inventory = current.certify_selected_against(
            &current,
            expected_sources
                .values()
                .map(|evidence| evidence.certificate.observation().source().clone())
                .collect(),
        )?;
        if current_inventory != *expected_inventory {
            return Ok(false);
        }
    }
    for (digest, evidence) in &expected_sources {
        let Some(leaf) = current_by_descriptor.get(digest).copied() else {
            return Ok(false);
        };
        if !leaf
            .source()
            .exact_descriptor_eq(evidence.certificate.observation().source())
        {
            return Ok(false);
        }
        if !adapter.revalidate_leaf(leaf, &evidence.certificate, evidence.checkpoint.as_ref())? {
            return Ok(false);
        }
    }
    for (digest, owned) in &owned_sources {
        if expected_sources.contains_key(digest) {
            continue;
        }
        if current_by_descriptor
            .get(digest)
            .is_some_and(|current| current.source().exact_descriptor_eq(owned))
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
    authorities: &[Arc<ProviderSourceRoot>],
    leaves: &[JsonlFamilyLeaf],
    rejected_leaves: &[JsonlFamilyRejectedLeaf],
) -> Result<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(FAMILY_INVENTORY_DOMAIN);
    digest.update([u8::from(missing)]);
    digest.update((leaves.len() as u64).to_be_bytes());
    digest.update((rejected_leaves.len() as u64).to_be_bytes());
    match authorities {
        [] => {}
        [authority] => {
            // Preserve the v1 single-root digest exactly. Multi-root adapters
            // use an explicit extension below without perturbing existing
            // providers' generation identities.
            digest.update(authority.authority_fingerprint());
        }
        authorities => {
            digest.update(b"multi-root-authorities-v1\0");
            digest.update((authorities.len() as u64).to_be_bytes());
            for authority in authorities {
                let path = authority.named_path().as_os_str().as_encoded_bytes();
                digest.update((path.len() as u64).to_be_bytes());
                digest.update(path);
                digest.update(authority.authority_fingerprint());
            }
        }
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

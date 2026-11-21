use std::{cmp::Ordering, collections::BTreeMap, fs::File, io::Read, path::Path, sync::Arc};

use ctx_history_core::{CertifiedSource, SourceKey};
use ctx_history_index_generation::{load_manifest_bytes, manifest_path, sha256_hex};
use serde::{Deserialize, Serialize};

use super::{capture_manifest_identity, hex_digest, PreparedManifest};
use crate::{
    is_generation_id, GenerationManifest, GenerationStateEnvelope, IndexError, Result,
    SourceCoreRecordAggregate, SourceRouteIdentity,
};

pub(super) const MANIFEST_FLAT_DELTA_STORAGE: &str = "ctx-manifest-flat-delta-v1";
pub(super) const MANIFEST_FLAT_DELTA_PREFIX: &[u8] =
    br#"{"storage_format":"ctx-manifest-flat-delta-v1","#;
pub(super) const MAX_MANIFEST_DELTA_CHANGES: usize = 1024;
pub(super) const MAX_MANIFEST_DELTA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredManifestFlatDeltaV1 {
    pub(super) storage_format: String,
    pub(super) base_generation_id: String,
    pub(super) indexed_documents: u64,
    pub(super) certified_source_bytes: u64,
    pub(super) source_count: usize,
    pub(super) changes: Vec<StoredManifestSourceChangeV1>,
    /// Sources the base does not have, sorted by identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) added: Vec<StoredManifestSourceChangeV1>,
    /// Members added to the base's routes, sorted by route identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) route_additions: Vec<StoredManifestRouteAdditionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) generation_state: Option<GenerationStateEnvelope>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredManifestSourceChangeV1 {
    pub(super) source_identity: [u8; 32],
    pub(super) source: CertifiedSource,
    pub(super) aggregate: SourceCoreRecordAggregate,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredManifestRouteAdditionV1 {
    pub(super) route_identity: SourceRouteIdentity,
    pub(super) sources: Vec<SourceKey>,
}

struct AccumulatedManifestDelta {
    anchor_generation_id: String,
    changes: BTreeMap<[u8; 32], StoredManifestSourceChangeV1>,
    added: BTreeMap<[u8; 32], StoredManifestSourceChangeV1>,
    route_additions: BTreeMap<SourceRouteIdentity, BTreeMap<[u8; 32], SourceKey>>,
    generation_state: Option<GenerationStateEnvelope>,
}

pub(super) fn decode_validated_delta(bytes: &[u8]) -> Result<StoredManifestFlatDeltaV1> {
    if bytes.len() > MAX_MANIFEST_DELTA_BYTES {
        return Err(IndexError::NonCanonicalManifest);
    }
    let delta: StoredManifestFlatDeltaV1 = serde_json::from_slice(bytes)?;
    let touched = delta
        .changes
        .len()
        .checked_add(delta.added.len())
        .ok_or(IndexError::NonCanonicalManifest)?;
    if serde_json::to_vec(&delta)? != bytes
        || delta.storage_format != MANIFEST_FLAT_DELTA_STORAGE
        || !is_generation_id(&delta.base_generation_id)
        || touched > MAX_MANIFEST_DELTA_CHANGES
        || (touched == 0 && delta.generation_state.is_none())
        || delta.added.is_empty() != delta.route_additions.is_empty()
    {
        return Err(IndexError::NonCanonicalManifest);
    }
    validate_sorted_source_changes(&delta.changes)?;
    validate_sorted_source_changes(&delta.added)?;
    let mut ci = 0;
    let mut ai = 0;
    while ci < delta.changes.len() && ai < delta.added.len() {
        match delta.changes[ci]
            .source_identity
            .cmp(&delta.added[ai].source_identity)
        {
            Ordering::Less => ci += 1,
            Ordering::Greater => ai += 1,
            Ordering::Equal => return Err(IndexError::NonCanonicalManifest),
        }
    }
    let mut previous_route: Option<&SourceRouteIdentity> = None;
    for addition in &delta.route_additions {
        if previous_route.is_some_and(|prev| prev >= &addition.route_identity)
            || addition.sources.is_empty()
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        previous_route = Some(&addition.route_identity);
        let mut previous_source: Option<[u8; 32]> = None;
        for source in &addition.sources {
            let digest = source.identity().digest();
            if previous_source.is_some_and(|prev| prev >= digest) {
                return Err(IndexError::NonCanonicalManifest);
            }
            previous_source = Some(digest);
        }
    }
    Ok(delta)
}

fn validate_sorted_source_changes(entries: &[StoredManifestSourceChangeV1]) -> Result<()> {
    let mut previous: Option<[u8; 32]> = None;
    for entry in entries {
        if previous.is_some_and(|digest| digest >= entry.source_identity) {
            return Err(IndexError::NonCanonicalManifest);
        }
        previous = Some(entry.source_identity);
        if entry.source.observation().source().identity().digest() != entry.source_identity {
            return Err(IndexError::NonCanonicalManifest);
        }
        if entry.aggregate.source_identity_digest() != hex_digest(entry.source_identity) {
            return Err(IndexError::NonCanonicalManifest);
        }
    }
    Ok(())
}

pub(super) fn materialize_delta(
    base: &GenerationManifest,
    delta: StoredManifestFlatDeltaV1,
) -> Result<GenerationManifest> {
    let expected_source_count = base
        .sources
        .len()
        .checked_add(delta.added.len())
        .ok_or(IndexError::NonCanonicalManifest)?;
    if delta.source_count != expected_source_count
        || base.sources.len() != base.core_record_aggregates.len()
    {
        return Err(IndexError::NonCanonicalManifest);
    }
    let mut replacements = Vec::with_capacity(delta.changes.len());
    for change in delta.changes {
        let source_index = base
            .sources
            .binary_search_by_key(&change.source_identity, |source| {
                source.observation().source().identity().digest()
            })
            .map_err(|_| IndexError::NonCanonicalManifest)?;
        let source_identity_hex = hex_digest(change.source_identity);
        let aggregate_index = base
            .core_record_aggregates
            .binary_search_by(|aggregate| {
                aggregate.source_identity_digest().cmp(&source_identity_hex)
            })
            .map_err(|_| IndexError::NonCanonicalManifest)?;
        if source_index != aggregate_index
            || base.core_record_aggregates[aggregate_index].source_identity_digest()
                != change.aggregate.source_identity_digest()
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        replacements.push((change.source, change.aggregate));
    }

    let mut additions = Vec::with_capacity(delta.added.len());
    for addition in delta.added {
        if base
            .sources
            .binary_search_by_key(&addition.source_identity, |source| {
                source.observation().source().identity().digest()
            })
            .is_ok()
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        let source_identity_hex = hex_digest(addition.source_identity);
        if base
            .core_record_aggregates
            .binary_search_by(|aggregate| {
                aggregate.source_identity_digest().cmp(&source_identity_hex)
            })
            .is_ok()
        {
            return Err(IndexError::NonCanonicalManifest);
        }
        additions.push((addition.source, addition.aggregate));
    }

    let route_additions = delta
        .route_additions
        .into_iter()
        .map(|entry| (entry.route_identity, entry.sources))
        .collect();

    let materialized = base.apply_validated_source_delta(
        replacements,
        additions,
        route_additions,
        delta.generation_state,
    )?;
    if materialized.indexed_documents != delta.indexed_documents
        || materialized.certified_source_bytes != delta.certified_source_bytes
        || materialized.sources.len() != delta.source_count
    {
        return Err(IndexError::NonCanonicalManifest);
    }
    Ok(materialized)
}

pub(super) fn prepare_flat_delta(
    root: &Path,
    manifest: &Arc<GenerationManifest>,
    base_generation_id: &str,
    base: &GenerationManifest,
) -> Result<Option<PreparedManifest>> {
    if base.sources.is_empty()
        || !is_generation_id(base_generation_id)
        || base.sources.len() != base.core_record_aggregates.len()
        || manifest.sources.len() != manifest.core_record_aggregates.len()
        || manifest.sources.len() < base.sources.len()
        || base.source_routes().len() != manifest.source_routes().len()
        || base.manifest_version != manifest.manifest_version
        || base.identity_version != manifest.identity_version
        || base.core_record_version != manifest.core_record_version
        || base.core_record_contract_fingerprint != manifest.core_record_contract_fingerprint
        || base.lexical_schema_version != manifest.lexical_schema_version
        || base.lexical_analyzer_version != manifest.lexical_analyzer_version
        || base.policy_schema_hash != manifest.policy_schema_hash
        || base.automatic_provider_discovery() != manifest.automatic_provider_discovery()
        || base.provider_root_config_digest() != manifest.provider_root_config_digest()
        || base.provider_roots() != manifest.provider_roots()
        || base.detached_released_provider_roots() != manifest.detached_released_provider_roots()
    {
        return Ok(None);
    }

    let mut step_changes = Vec::new();
    let mut step_added = Vec::new();
    let mut base_index = 0;
    let mut current_index = 0;
    while base_index < base.sources.len() && current_index < manifest.sources.len() {
        let base_source = &base.sources[base_index];
        let base_aggregate = &base.core_record_aggregates[base_index];
        let current_source = &manifest.sources[current_index];
        let current_aggregate = &manifest.core_record_aggregates[current_index];
        let base_identity = base_source.observation().source().identity().digest();
        let current_identity = current_source.observation().source().identity().digest();
        match base_identity.cmp(&current_identity) {
            Ordering::Less => return Ok(None),
            Ordering::Greater => {
                if current_aggregate.source_identity_digest() != hex_digest(current_identity) {
                    return Ok(None);
                }
                step_added.push(StoredManifestSourceChangeV1 {
                    source_identity: current_identity,
                    source: current_source.clone(),
                    aggregate: current_aggregate.clone(),
                });
                if step_changes.len() + step_added.len() > MAX_MANIFEST_DELTA_CHANGES {
                    return Ok(None);
                }
                current_index += 1;
            }
            Ordering::Equal => {
                if !base_source
                    .observation()
                    .source()
                    .exact_descriptor_eq(current_source.observation().source())
                    || base_aggregate.source_identity_digest()
                        != current_aggregate.source_identity_digest()
                    || current_aggregate.source_identity_digest() != hex_digest(current_identity)
                {
                    return Ok(None);
                }
                let source_changed = !base_source.shares_immutable_parts_with(current_source)
                    && base_source != current_source;
                if source_changed || base_aggregate != current_aggregate {
                    step_changes.push(StoredManifestSourceChangeV1 {
                        source_identity: current_identity,
                        source: current_source.clone(),
                        aggregate: current_aggregate.clone(),
                    });
                    if step_changes.len() + step_added.len() > MAX_MANIFEST_DELTA_CHANGES {
                        return Ok(None);
                    }
                }
                base_index += 1;
                current_index += 1;
            }
        }
    }
    if base_index < base.sources.len() {
        return Ok(None);
    }
    for (current_source, current_aggregate) in manifest.sources[current_index..]
        .iter()
        .zip(&manifest.core_record_aggregates[current_index..])
    {
        let current_identity = current_source.observation().source().identity().digest();
        if current_aggregate.source_identity_digest() != hex_digest(current_identity) {
            return Ok(None);
        }
        step_added.push(StoredManifestSourceChangeV1 {
            source_identity: current_identity,
            source: current_source.clone(),
            aggregate: current_aggregate.clone(),
        });
        if step_changes.len() + step_added.len() > MAX_MANIFEST_DELTA_CHANGES {
            return Ok(None);
        }
    }

    let mut step_route_additions: Vec<(SourceRouteIdentity, Vec<SourceKey>)> = Vec::new();
    let mut total_route_additions = 0usize;
    for (base_route, current_route) in base.source_routes().iter().zip(manifest.source_routes()) {
        if base_route.route_identity() != current_route.route_identity()
            || base_route.missing_state() != current_route.missing_state()
        {
            return Ok(None);
        }
        let base_members = base_route.sources();
        let current_members = current_route.sources();
        if std::ptr::eq(base_members.as_ptr(), current_members.as_ptr())
            && base_members.len() == current_members.len()
        {
            continue;
        }
        if current_members.len() < base_members.len() {
            return Ok(None);
        }
        let mut added_for_route = Vec::new();
        let mut bi = 0;
        let mut ci = 0;
        while bi < base_members.len() && ci < current_members.len() {
            let base_key = base_members[bi].identity().digest();
            let current_key = current_members[ci].identity().digest();
            match base_key.cmp(&current_key) {
                Ordering::Less => return Ok(None),
                Ordering::Greater => {
                    if step_added
                        .binary_search_by_key(&current_key, |entry| entry.source_identity)
                        .is_err()
                    {
                        return Ok(None);
                    }
                    added_for_route.push(current_members[ci].clone());
                    ci += 1;
                }
                Ordering::Equal => {
                    if !base_members[bi].exact_descriptor_eq(&current_members[ci]) {
                        return Ok(None);
                    }
                    bi += 1;
                    ci += 1;
                }
            }
        }
        if bi < base_members.len() {
            return Ok(None);
        }
        for member in &current_members[ci..] {
            let current_key = member.identity().digest();
            if step_added
                .binary_search_by_key(&current_key, |entry| entry.source_identity)
                .is_err()
            {
                return Ok(None);
            }
            added_for_route.push(member.clone());
        }
        if !added_for_route.is_empty() {
            total_route_additions += added_for_route.len();
            step_route_additions.push((base_route.route_identity().clone(), added_for_route));
        }
    }
    if total_route_additions != step_added.len() {
        return Ok(None);
    }

    let step_generation_state_changed = base.generation_state() != manifest.generation_state();
    if step_generation_state_changed && manifest.generation_state().is_none() {
        return Ok(None);
    }
    if step_changes.is_empty() && step_added.is_empty() && !step_generation_state_changed {
        return Ok(None);
    }

    let mut accumulated = accumulated_manifest_delta(root, base_generation_id)?;
    for change in step_changes {
        if let std::collections::btree_map::Entry::Occupied(mut entry) =
            accumulated.added.entry(change.source_identity)
        {
            entry.insert(change);
        } else {
            accumulated.changes.insert(change.source_identity, change);
        }
    }
    for addition in step_added {
        if accumulated.changes.contains_key(&addition.source_identity)
            || accumulated.added.contains_key(&addition.source_identity)
        {
            return Ok(None);
        }
        accumulated.added.insert(addition.source_identity, addition);
    }
    for (route_identity, sources) in step_route_additions {
        let entry = accumulated
            .route_additions
            .entry(route_identity)
            .or_default();
        for source in sources {
            let digest = source.identity().digest();
            if entry.insert(digest, source).is_some() {
                return Ok(None);
            }
        }
    }
    if step_generation_state_changed {
        accumulated.generation_state = manifest.generation_state().cloned();
    }

    let total_touched = accumulated.changes.len() + accumulated.added.len();
    if total_touched > MAX_MANIFEST_DELTA_CHANGES
        || (total_touched == 0 && accumulated.generation_state.is_none())
    {
        return Ok(None);
    }

    let base_fence = Some((
        accumulated.anchor_generation_id.clone(),
        capture_manifest_identity(root, &accumulated.anchor_generation_id)?,
    ));
    let route_additions = accumulated
        .route_additions
        .into_iter()
        .map(|(route_identity, sources)| StoredManifestRouteAdditionV1 {
            route_identity,
            sources: sources.into_values().collect(),
        })
        .collect();
    let anchor_generation_id = accumulated.anchor_generation_id;
    let delta = StoredManifestFlatDeltaV1 {
        storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
        base_generation_id: anchor_generation_id.clone(),
        indexed_documents: manifest.indexed_documents,
        certified_source_bytes: manifest.certified_source_bytes,
        source_count: manifest.sources.len(),
        changes: accumulated.changes.into_values().collect(),
        added: accumulated.added.into_values().collect(),
        route_additions,
        generation_state: accumulated.generation_state,
    };
    let bytes = serde_json::to_vec(&delta)?;
    if bytes.len() > MAX_MANIFEST_DELTA_BYTES {
        return Ok(None);
    }
    Ok(Some(PreparedManifest::new(
        sha256_hex(&bytes),
        Some(anchor_generation_id),
        bytes,
        Arc::clone(manifest),
        base_fence,
    )?))
}

fn accumulated_manifest_delta(
    root: &Path,
    generation_id: &str,
) -> Result<AccumulatedManifestDelta> {
    let mut prefix = [0_u8; 64];
    let mut file = File::open(manifest_path(root, generation_id)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            IndexError::MissingManifest(generation_id.to_owned())
        } else {
            IndexError::Io(error)
        }
    })?;
    let prefix_len = file.read(&mut prefix)?;
    let prefix = &prefix[..prefix_len];
    if !prefix.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        // `base` was materialized through the authenticated loader before this
        // helper was called, so reaching its non-delta anchor does not require
        // reading and hashing the potentially corpus-sized full manifest again.
        return Ok(AccumulatedManifestDelta {
            anchor_generation_id: generation_id.to_owned(),
            changes: BTreeMap::new(),
            added: BTreeMap::new(),
            route_additions: BTreeMap::new(),
            generation_state: None,
        });
    }

    let bytes = load_manifest_bytes(root, generation_id)?;
    if bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        let delta = decode_validated_delta(&bytes)?;
        let changes = delta
            .changes
            .into_iter()
            .map(|change| (change.source_identity, change))
            .collect();
        let added = delta
            .added
            .into_iter()
            .map(|addition| (addition.source_identity, addition))
            .collect();
        let route_additions = delta
            .route_additions
            .into_iter()
            .map(|entry| {
                let members = entry
                    .sources
                    .into_iter()
                    .map(|source| (source.identity().digest(), source))
                    .collect();
                (entry.route_identity, members)
            })
            .collect();
        return Ok(AccumulatedManifestDelta {
            anchor_generation_id: delta.base_generation_id,
            changes,
            added,
            route_additions,
            generation_state: delta.generation_state,
        });
    }
    Err(IndexError::NonCanonicalManifest)
}

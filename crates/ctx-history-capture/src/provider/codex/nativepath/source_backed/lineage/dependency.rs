use sha2::{Digest, Sha256};

use super::*;

// V4 binds the complete normalized lineage tuple into warm replay.
const LINEAGE_DEPENDENCY_DOMAIN: &[u8] = b"ctx/codex-lineage-dependency/v4\0";

pub(super) fn compute_dependency_digests(
    nodes: &mut [LineageNodeV0],
) -> CodexSourceBackedResultV0<usize> {
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

pub(super) fn digest_marker(marker: &[u8]) -> [u8; 32] {
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

use super::*;
use ctx_history_provider_runtime::ProviderJsonlMembershipObservation;

pub(super) fn observe(
    scope: SourceAnchorScope,
    root: &Path,
    opening: &ProviderJsonlInventory,
) -> Result<ProviderJsonlMembershipObservation> {
    if opening.root_missing() {
        return ProviderJsonlMembershipObservation::observe(root, opening);
    }
    let mut membership = ProviderJsonlMembershipObservation::observe_frozen_authorities(opening)?;
    let claimed = opening
        .members()
        .iter()
        .filter_map(|member| member.source())
        .map(SourceKey::exact_descriptor_digest)
        .collect::<std::collections::HashSet<_>>();
    let mut admitted_aliases = std::collections::HashSet::new();
    // First-record classification preserves the selected copy's binding when
    // it becomes pending. Its existing aliases still have exact dependencies.
    let bindings = opening
        .accepted_leaves()
        .map(|leaf| (leaf.source(), leaf.binding()))
        .chain(
            opening
                .pending_leaves()
                .filter_map(|leaf| leaf.source().map(|source| (source, leaf.binding()))),
        );
    for (source, proof) in bindings {
        let binding = decode_binding_proof(proof)?;
        for alias in binding.alias_route_sha256 {
            admitted_aliases.insert((source.exact_descriptor_digest(), alias));
        }
    }
    let unbound = membership
        .unbound_routes()
        .map(|(path, _, _)| path.to_path_buf())
        .collect::<Vec<_>>();
    for path in unbound {
        let Some(native_id) = native_session_id(&path) else {
            continue;
        };
        let source = source_key_scoped(native_id, scope)?;
        let digest = source.exact_descriptor_digest();
        if claimed.contains(&digest)
            && !admitted_aliases.contains(&(digest, cursor_route_sha256(&path)))
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        membership.bind_source_hint(path, source);
    }
    Ok(membership)
}

// The native layout binds identity without parsing transcript bytes. Unknown
// JSON files under the authority do not become transcript ownership hints.
fn native_session_id(path: &Path) -> Option<&str> {
    let session = path.parent()?;
    let transcripts = session.parent()?;
    let projects = transcripts.parent()?.parent()?;
    if transcripts.file_name()? != "agent-transcripts" || projects.file_name()? != "projects" {
        return None;
    }
    let id = session
        .file_name()?
        .to_str()
        .filter(|id| !id.trim().is_empty())?;
    (path.file_name()?.to_str()? == format!("{id}.jsonl")).then_some(id)
}

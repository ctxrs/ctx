use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorBinding {
    pub(super) native_session_id: String,
    pub(super) logical_transcript_sha256: Option<[u8; 32]>,
    pub(super) selected_route_sha256: [u8; 32],
    pub(super) alias_route_sha256: Vec<[u8; 32]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) alternative_relative_path: Option<PathBuf>,
}

pub(super) fn validate_binding(
    leaf: &JsonlFamilyLeaf,
    binding: &CursorBinding,
    _source_file: &OpenedProviderSourceFile,
    source_anchor_scope: SourceAnchorScope,
) -> Result<()> {
    let expected_source = match &binding.alternative_relative_path {
        Some(relative) => {
            if relative != leaf.authority_path()
                || leaf.authority().named_path().join(relative) != leaf.source_path()
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            alternative_source_key(&binding.native_session_id, relative, source_anchor_scope)?
        }
        None => source_key_scoped(&binding.native_session_id, source_anchor_scope)?,
    };
    if !expected_source.exact_descriptor_eq(leaf.source())
        || cursor_route_sha256(leaf.source_path()) != binding.selected_route_sha256
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

pub(super) fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<CursorBinding> {
    decode_binding_proof(leaf.binding())
}

pub(super) fn decode_binding_proof(proof: &TypedKey) -> Result<CursorBinding> {
    let TypedKey::Bytes(bytes) = proof else {
        return Err(contract("Cursor family binding is malformed"));
    };
    Ok(serde_json::from_slice(bytes)?)
}

pub(super) fn alternative_source_key(
    native_session_id: &str,
    relative_path: &Path,
    scope: SourceAnchorScope,
) -> Result<SourceKey> {
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(contract(
            "Cursor alternative provenance must be root-relative",
        ));
    }
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Cursor.as_str(),
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        ALTERNATIVE_ANCHOR_NAMESPACE,
        TypedKey::composite(vec![
            TypedKey::utf8(native_session_id).map_err(contract)?,
            TypedKey::bytes(relative_path.as_os_str().as_encoded_bytes().to_vec())
                .map_err(contract)?,
        ])
        .map_err(contract)?,
        scope,
    )
    .map_err(contract)
}

pub(super) fn alternative_native_session(source: &SourceKey) -> Option<&str> {
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return None;
    };
    if namespace != ALTERNATIVE_ANCHOR_NAMESPACE {
        return None;
    }
    let TypedKey::Composite(parts) = key else {
        return None;
    };
    let parts = match parts.as_slice() {
        [TypedKey::Bytes(_), TypedKey::Composite(scoped)] => scoped.as_slice(),
        parts => parts,
    };
    match parts {
        [TypedKey::Utf8(native), TypedKey::Bytes(_)] => Some(native),
        _ => None,
    }
}

pub(super) fn prior_cursor_route_index(
    base_path: &Path,
    routes: &[super::super::layout::CursorTranscriptPath],
) -> Option<usize> {
    if let Some(index) = routes.iter().position(|route| route.path() == base_path) {
        return Some(index);
    }
    // Named-root relocation keeps relative provenance. Never resolve an
    // ambiguous suffix, and never transfer a deleted canonical to a sibling.
    let mut matches = routes
        .iter()
        .enumerate()
        .filter(|(_, route)| base_path.ends_with(route.authority_relative_path()))
        .map(|(index, _)| index);
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

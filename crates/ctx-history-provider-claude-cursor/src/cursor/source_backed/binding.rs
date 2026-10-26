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

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorBinding {
    pub(super) native_session_id: String,
    pub(super) logical_transcript_sha256: Option<[u8; 32]>,
    pub(super) selected_route_sha256: [u8; 32],
    pub(super) alias_route_sha256: Vec<[u8; 32]>,
}

pub(super) fn validate_binding(
    leaf: &JsonlFamilyLeaf,
    binding: &CursorBinding,
    _source_file: &OpenedProviderSourceFile,
) -> Result<()> {
    if !source_key(&binding.native_session_id)?.exact_descriptor_eq(leaf.source())
        || cursor_route_sha256(leaf.source_path()) != binding.selected_route_sha256
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

pub(super) fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<CursorBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(contract("Cursor family binding is malformed"));
    };
    Ok(serde_json::from_slice(bytes)?)
}

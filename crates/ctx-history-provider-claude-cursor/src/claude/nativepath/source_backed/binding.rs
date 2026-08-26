use super::*;
use ctx_history_source_io::{
    NON_REGULAR_PROVIDER_SOURCE_REASON, REPARSE_PROVIDER_SOURCE_REASON,
    SYMLINK_PROVIDER_SOURCE_REASON,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub(super) project_dir: PathBuf,
    pub(super) source_root_lineage: Option<[u8; 32]>,
    pub(super) key: ClaudeSessionKey,
    pub(super) layout: SessionLayout,
}

pub(super) fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<Binding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(contract("Claude family binding is malformed"));
    };
    Ok(serde_json::from_slice(bytes)?)
}

pub(super) fn relative_to_authority(
    authority: &ProviderSourceRoot,
    path: &Path,
) -> Result<PathBuf> {
    path.strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Claude transcripts must remain below their selected authority",
        })
}

pub(super) fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(super) fn is_quarantinable_claude_leaf_error(error: &CaptureError) -> bool {
    matches!(error, CaptureError::Io(source) if source.kind() == io::ErrorKind::PermissionDenied)
        || matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { reason, .. }
                if *reason == SYMLINK_PROVIDER_SOURCE_REASON
                    || *reason == REPARSE_PROVIDER_SOURCE_REASON
                    || *reason == NON_REGULAR_PROVIDER_SOURCE_REASON
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaudeSourceClaim {
    New,
    Duplicate,
}

pub(super) fn claim_claude_source(
    claimed: &mut HashMap<[u8; 32], SourceKey>,
    source: &SourceKey,
) -> Result<ClaudeSourceClaim> {
    let digest = source.exact_descriptor_digest();
    if let Some(previous) = claimed.get(&digest) {
        if previous.exact_descriptor_eq(source) {
            return Ok(ClaudeSourceClaim::Duplicate);
        }
        return Err(CaptureError::InvalidPayload(
            "Claude source descriptor digest collision".to_owned(),
        ));
    }
    claimed.insert(digest, source.clone());
    Ok(ClaudeSourceClaim::New)
}

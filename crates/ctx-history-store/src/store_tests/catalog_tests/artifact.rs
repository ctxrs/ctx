#[allow(unused_imports)]
use super::*;

pub(crate) fn artifact_record(id: Uuid, byte_size: u64) -> Artifact {
    Artifact {
        id,
        kind: ArtifactKind::Markdown,
        blob_hash: format!("{:064x}", 1),
        blob_path: format!("{OBJECTS_DIR}/00/test-artifact"),
        byte_size,
        media_type: Some("text/markdown".to_owned()),
        preview_text: Some("artifact preview".to_owned()),
        redaction_state: RedactionState::LocalPreview,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    }
}

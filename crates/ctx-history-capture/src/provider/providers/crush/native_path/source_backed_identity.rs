use super::*;

pub(super) fn crush_source_key(project_key: TypedKey) -> CrushSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(CRUSH_SOURCE_ANCHOR_NAMESPACE, project_key)?;
    Ok(SourceKey::derive(
        CaptureProvider::Crush.as_str(),
        CRUSH_SQLITE_SOURCE_FORMAT,
        CRUSH_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

pub(super) fn crush_source_revision(
    evidence: &SqliteSourceEvidence,
    schema_fingerprint: &str,
) -> String {
    format!(
        "crush-sqlite-snapshot-v1:capture={CRUSH_CAPTURE_REVISION};policy={CRUSH_POLICY_REVISION};schema={schema_fingerprint};{}",
        sqlite_evidence_revision_component(evidence),
    )
}

fn sqlite_evidence_revision_component(evidence: &SqliteSourceEvidence) -> String {
    format!(
        "identity={};length={};revision={}",
        hex_bytes(evidence.identity()),
        evidence.length(),
        hex_bytes(evidence.revision()),
    )
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

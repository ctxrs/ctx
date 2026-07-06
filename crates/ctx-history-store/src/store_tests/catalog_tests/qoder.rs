#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v37_adds_qoder_provider_checks() {
    assert_provider_migration_accepts(
        36,
        "qoder",
        "qoder_transcript_jsonl_tree",
        "/tmp/qoder/projects",
        "/tmp/qoder/projects/workspace/transcript/session.jsonl",
    );
}

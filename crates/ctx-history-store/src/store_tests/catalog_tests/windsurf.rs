#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v27_adds_windsurf_provider_checks() {
    assert_provider_migration_accepts(
        26,
        "windsurf",
        "windsurf_cascade_hook_transcript_jsonl",
        "/tmp/windsurf/transcripts",
        "/tmp/windsurf/transcripts/trajectory.jsonl",
    );
}

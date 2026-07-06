#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn bounded_probe_reports_budget_exhausted_source_as_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let claude = temp.path().join(".claude/projects");
    std::fs::create_dir_all(&claude).unwrap();
    for index in 0..10_001 {
        std::fs::create_dir(claude.join(format!("project-{index:05}"))).unwrap();
    }

    assert_source_status(
        temp.path(),
        CaptureProvider::Claude,
        ProviderSourceStatus::Unknown,
    );
}

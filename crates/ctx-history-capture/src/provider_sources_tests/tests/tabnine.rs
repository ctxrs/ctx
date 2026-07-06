#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn tabnine_default_source_is_empty_until_chat_transcripts_exist() {
    let temp = tempfile::tempdir().unwrap();
    let tabnine = temp.path().join(".tabnine/agent");
    std::fs::create_dir_all(&tabnine).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Tabnine)
        .unwrap();
    assert!(source.exists);
    assert_eq!(source.status, ProviderSourceStatus::Empty);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert!(source
        .unsupported_reason
        .unwrap()
        .contains("no Tabnine CLI chat JSONL transcripts"));

    let chats = tabnine.join("tmp/project/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(
        chats.join("session-2026-07-05T12-00-00000000.jsonl"),
        "{}\n",
    )
    .unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Tabnine)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn gemini_default_source_is_empty_until_chat_transcripts_exist() {
    let temp = tempfile::tempdir().unwrap();
    let gemini = temp.path().join(".gemini");
    std::fs::create_dir_all(&gemini).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Gemini)
        .unwrap();
    assert!(source.exists);
    assert_eq!(source.status, ProviderSourceStatus::Empty);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert!(source
        .unsupported_reason
        .unwrap()
        .contains("no Gemini CLI chat JSONL transcripts"));

    let chats = gemini.join("tmp/project/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(chats.join("session.jsonl"), "{}\n").unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Gemini)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

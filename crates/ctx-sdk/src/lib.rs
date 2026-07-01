//! In-process Rust SDK for ctx local agent history.
//!
//! The SDK is a small facade over the existing ctx history crates. It opens the
//! local SQLite store directly and calls provider import/search functions in the
//! same process. It intentionally does not shell out to the `ctx` binary or parse
//! CLI JSON.

mod client;
mod error;
mod import;
mod search;
mod transcript;

pub use client::{CtxClient, CtxClientBuilder, CtxPaths, DoctorReport, Status};
pub use error::{Error, Result};
pub use import::{ImportOptions, ImportReport, ImportSourceReport, ImportTotals, SourceStats};
pub use search::SearchOptions;
pub use transcript::{
    EventLocation, EventWindow, EventWindowOptions, SessionLocation, SessionTranscript,
    ShowSessionOptions, TranscriptMode,
};

pub use ctx_history_capture::{
    CodexEventImportMode, CodexSessionImportProgressCallback, CodexToolOutputMode,
    ProviderCatalogSupport, ProviderImportSummary, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceStatus,
};
pub use ctx_history_core::{
    CaptureProvider, CaptureSource, Event, EventRole, EventType, HistoryRecord, Session,
};
pub use ctx_history_search::{
    PacketOptions, SearchFilters, SearchPacket, SearchResultMode, SearchResultScope,
};

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::*;
    use chrono::Utc;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history")
            .join(name)
    }

    #[test]
    fn status_is_read_only_until_init() {
        let temp = tempfile::tempdir().unwrap();
        let client = CtxClient::with_data_root(temp.path());

        let status = client.status().unwrap();
        assert!(!status.initialized);
        assert!(!status.paths.database_path.exists());

        let missing_doctor = client.doctor().unwrap();
        assert!(!missing_doctor.ok);
        assert!(missing_doctor
            .findings
            .iter()
            .any(|finding| finding.contains("database does not exist")));
        assert!(matches!(
            client.search("anything", SearchOptions::default()),
            Err(Error::StoreNotInitialized(_))
        ));

        let initialized = client.init().unwrap();
        assert!(initialized.initialized);
        assert!(initialized.paths.database_path.exists());

        let healthy_doctor = client.doctor().unwrap();
        assert!(healthy_doctor.ok, "{:?}", healthy_doctor.findings);
    }

    #[test]
    fn imports_searches_and_loads_history_in_process() {
        let temp = tempfile::tempdir().unwrap();
        let client = CtxClient::builder()
            .data_root(temp.path())
            .home_dir(temp.path())
            .build()
            .unwrap();

        let report = client
            .import_path(
                CaptureProvider::Codex,
                fixture("codex-sessions"),
                ImportOptions::default(),
            )
            .unwrap();
        assert_eq!(report.totals.failed_sources, 0);
        assert!(report.totals.imported_sessions > 0);
        assert!(report.totals.imported_events > 0);

        let packet = client
            .search(
                "onboarding",
                SearchOptions::default()
                    .provider(CaptureProvider::Codex)
                    .events()
                    .limit(5),
            )
            .unwrap();
        assert_eq!(packet.query, "onboarding");
        let first = packet.results.first().expect("search should return a hit");
        let event_id = first.event_id.expect("search result should carry event id");
        let session_id = first
            .session_id
            .expect("search result should carry session id");

        let window = client
            .show_event(event_id, EventWindowOptions::window(1))
            .unwrap();
        assert_eq!(window.event.id, event_id);
        assert!(!window.events.is_empty());

        let transcript = client
            .show_session(session_id, ShowSessionOptions::default())
            .unwrap();
        assert_eq!(transcript.session.id, session_id);
        assert_eq!(transcript.mode, TranscriptMode::Lite);
        assert!(!transcript.events.is_empty());

        let event_location = client.locate_event(event_id).unwrap();
        assert_eq!(event_location.event.id, event_id);
        assert_eq!(event_location.session.unwrap().id, session_id);

        let session_location = client.locate_session(session_id).unwrap();
        assert_eq!(session_location.session.id, session_id);
        assert!(session_location.source.is_some());

        let full = client
            .show_session(
                session_id,
                ShowSessionOptions {
                    mode: TranscriptMode::Full,
                },
            )
            .unwrap();
        assert_eq!(TranscriptMode::Full.as_str(), "full");
        assert_eq!(TranscriptMode::Lite.as_str(), "lite");
        assert_eq!(TranscriptMode::Log.as_str(), "log");
        assert!(full
            .events
            .iter()
            .all(|event| event.event_type == EventType::Message));

        let around = client
            .show_event(
                event_id,
                EventWindowOptions {
                    before: 1,
                    after: 1,
                    window: None,
                },
            )
            .unwrap();
        assert!(around.events.len() >= 2);
    }

    #[test]
    fn rich_codex_llm_fixture_preserves_tool_events_for_log_transcripts() {
        let temp = tempfile::tempdir().unwrap();
        let client = CtxClient::with_data_root(temp.path());

        let report = client
            .import_path(
                CaptureProvider::Codex,
                fixture("codex-rich-sessions"),
                ImportOptions::default().rich_codex(),
            )
            .unwrap();
        assert_eq!(report.totals.failed_sources, 0);
        assert!(report.totals.imported_events > 0);

        let packet = client
            .search(
                "redacted sample app",
                SearchOptions::default()
                    .provider(CaptureProvider::Codex)
                    .events()
                    .limit(5),
            )
            .unwrap();
        let session_id = packet.results[0]
            .session_id
            .expect("real LLM fixture hit should include session id");

        let log = client
            .show_session(
                session_id,
                ShowSessionOptions {
                    mode: TranscriptMode::Log,
                },
            )
            .unwrap();
        assert!(log
            .events
            .iter()
            .any(|event| event.event_type == EventType::ToolCall));
        assert!(log
            .events
            .iter()
            .any(|event| event.event_type == EventType::ToolOutput));

        let lite = client
            .show_session(session_id, ShowSessionOptions::default())
            .unwrap();
        assert!(log.events.len() > lite.events.len());
    }

    #[test]
    fn default_source_discovery_imports_available_llm_histories() {
        let temp = tempfile::tempdir().unwrap();
        copy_dir_all(
            &fixture("codex-sessions"),
            &temp.path().join(".codex/sessions"),
        );
        fs::create_dir_all(temp.path().join(".pi")).unwrap();
        fs::copy(
            fixture("pi-session.jsonl"),
            temp.path().join(".pi/sessions.jsonl"),
        )
        .unwrap();

        let client = CtxClient::builder()
            .data_root(temp.path().join("ctx-data"))
            .home_dir(temp.path())
            .build()
            .unwrap();
        let sources = client.sources();
        assert!(sources
            .iter()
            .any(|source| source.provider == CaptureProvider::Codex
                && source.status == ProviderSourceStatus::Available));
        assert!(client
            .sources_for_provider(CaptureProvider::Pi)
            .iter()
            .any(|source| source.status == ProviderSourceStatus::Available));

        let report = client
            .import_available_sources(None, ImportOptions::default())
            .unwrap();
        assert_eq!(report.totals.failed_sources, 0);
        assert!(report.totals.imported_sources >= 2);
        assert!(report.totals.source_files > 0);

        let status = client.status().unwrap();
        assert!(status.indexed_items > 0);
        assert!(status.indexed_sources >= 2);
    }

    #[test]
    fn unsupported_and_empty_imports_report_typed_errors() {
        let temp = tempfile::tempdir().unwrap();
        let client = CtxClient::builder()
            .data_root(temp.path().join("ctx-data"))
            .home_dir(temp.path())
            .build()
            .unwrap();

        assert!(matches!(
            client.import_sources(Vec::new(), ImportOptions::default()),
            Err(Error::NoImportableSources)
        ));
        assert!(matches!(
            client.import_available_sources(Some(CaptureProvider::Codex), ImportOptions::default()),
            Err(Error::NoImportableSources)
        ));

        let unsupported = client.source_for_path(CaptureProvider::Shell, temp.path().join("shell"));
        let report = client
            .import_sources(vec![unsupported.clone()], ImportOptions::default())
            .unwrap();
        assert_eq!(report.totals.failed_sources, 1);
        assert!(report.sources[0].error.is_some());

        assert!(matches!(
            client.import_sources(vec![unsupported], ImportOptions::default().fail_fast()),
            Err(Error::UnsupportedProviderImport { .. })
        ));

        let missing_codex = client
            .import_path(
                CaptureProvider::Codex,
                temp.path().join("missing-session.jsonl"),
                ImportOptions::default(),
            )
            .unwrap();
        assert_eq!(missing_codex.totals.failed_sources, 1);
        assert_eq!(missing_codex.sources[0].stats, SourceStats::default());
        assert!(missing_codex.sources[0].error.is_some());
    }

    #[test]
    fn search_options_builder_sets_all_filters() {
        let session = uuid::Uuid::new_v4();
        let since = Utc::now();
        let options = SearchOptions::default()
            .limit(7)
            .provider(CaptureProvider::Codex)
            .workspace("/repo/app")
            .since(since)
            .file("src/main.rs")
            .session(session)
            .term("onboarding")
            .term("migration");

        assert_eq!(options.packet.limit, 7);
        assert_eq!(
            options.packet.filters.provider,
            Some(CaptureProvider::Codex)
        );
        assert_eq!(options.packet.filters.repo.as_deref(), Some("/repo/app"));
        assert_eq!(options.packet.filters.since, Some(since));
        assert_eq!(options.packet.filters.file.as_deref(), Some("src/main.rs"));
        assert_eq!(options.packet.filters.session, Some(session));
        assert_eq!(options.packet.result_mode, SearchResultMode::Events);
        assert_eq!(options.terms, ["onboarding", "migration"]);
    }

    fn copy_dir_all(source: &Path, target: &Path) {
        fs::create_dir_all(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            let target_path = target.join(entry.file_name());
            if file_type.is_dir() {
                copy_dir_all(&entry.path(), &target_path);
            } else {
                fs::copy(entry.path(), target_path).unwrap();
            }
        }
    }
}

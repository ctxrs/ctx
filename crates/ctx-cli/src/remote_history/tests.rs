use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use clap::{CommandFactory, FromArgMatches, Parser};
use ctx_agent_integrations::tool_backend::{ToolBackend, ToolOperation};
use ctx_history_core::{
    derive_event_id, derive_session_id, CoreContentPolicyStatus, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
};
use ctx_history_server::{
    Citation, CitationKind, CollectionStatus, HostedEvent, HostedSearchHit, Provenance,
    SearchResponse,
};
use ctx_history_sharing::{Connection, Credentials, Endpoint, RemoteClient};
use serde_json::{json, Value};

use super::*;

#[test]
fn mcp_serve_help_describes_local_and_shared_access() {
    let help = crate::Cli::try_parse_from(["ctx", "--server", "team", "mcp", "serve", "--help"])
        .unwrap_err();
    assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    let text = help
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("By default, tools execute locally"));
    assert!(text.contains("With --server NAME"));
    assert!(text.contains("saved shared collection"));
    assert!(text.contains("Local-only graph snapshot"));
}

#[test]
fn remote_filters_fail_explicitly_before_any_connection_or_local_state() {
    let parsed = crate::Cli::try_parse_from([
        "ctx",
        "--server",
        "team",
        "search",
        "needle",
        "--workspace",
        "secret",
    ])
    .unwrap();
    let CommandRoot::Search(args) = parsed.command else {
        panic!("search expected")
    };
    assert!(validation::cli_search(&args)
        .unwrap_err()
        .to_string()
        .contains("not supported"));
    let parsed = crate::Cli::try_parse_from([
        "ctx",
        "--server",
        "team",
        "search",
        "needle",
        "--backend",
        "lexical",
    ])
    .unwrap();
    let CommandRoot::Search(args) = parsed.command else {
        panic!("search expected")
    };
    validation::cli_search(&args).unwrap();
}

#[test]
fn remote_backend_never_turns_a_local_only_tool_into_a_local_read() {
    let error = remote_backend()
        .execute(ToolOperation::Sources)
        .unwrap_err();
    assert!(error.error.to_string().contains("local-only"));
}

pub(super) const COLLECTION: &str = "00000000-0000-4000-8000-000000000001";

pub(super) fn remote_backend() -> RemoteBackend {
    RemoteBackend::new(
        RemoteClient::new(
            Connection {
                endpoint: Endpoint::parse("http://127.0.0.1:1").unwrap(),
                collection: COLLECTION.into(),
            },
            Credentials::read_only("synthetic-test-token".into()).unwrap(),
        )
        .unwrap(),
    )
}

#[test]
fn cli_remote_default_is_log_and_explicit_transcript_modes_are_not_discarded() {
    for (mode, supported) in [
        (None, true),
        (Some("log"), true),
        (Some("lite"), false),
        (Some("full"), false),
    ] {
        let citation = fixture().session_citation;
        let mut argv = vec!["ctx", "--server", "team", "show", "session", &citation];
        if let Some(mode) = mode {
            argv.extend(["--mode", mode]);
        }
        let matches = crate::Cli::command().try_get_matches_from(argv).unwrap();
        let mut cli = crate::Cli::from_arg_matches(&matches).unwrap();
        let CommandRoot::Show(args) = &mut cli.command else {
            panic!("show")
        };
        validation::select_cli_mode(args, &matches);
        assert_eq!(validation::cli_show(args).is_ok(), supported, "{mode:?}");
        if supported {
            let ShowTarget::Session(args) = &args.target else {
                panic!("session")
            };
            assert_eq!(args.mode, crate::transcript::TranscriptMode::Log);
        }
    }
    let cli = crate::Cli::try_parse_from(["ctx", "show", "session", "deadbeef"]).unwrap();
    let CommandRoot::Show(ShowArgs {
        target: ShowTarget::Session(args),
    }) = cli.command
    else {
        panic!("session")
    };
    assert_eq!(args.mode, crate::transcript::TranscriptMode::Lite);
}

#[test]
fn canonical_citations_are_kind_and_collection_bound_without_echoing_invalid_input() {
    let event = fixture();
    validation::citation(&event.citation, CitationKind::Event, COLLECTION).unwrap();
    validation::citation(&event.session_citation, CitationKind::Session, COLLECTION).unwrap();
    assert!(
        validation::citation(&event.session_citation, CitationKind::Event, COLLECTION).is_err()
    );
    assert!(validation::citation(
        &event.citation,
        CitationKind::Event,
        "00000000-0000-4000-8000-000000000002"
    )
    .is_err());
    for selector in [
        "deadbeef",
        "ctxh1_invalid-secret-\u{202e}",
        "00000000-0000-4000-8000-000000000001",
    ] {
        let error = validation::citation(selector, CitationKind::Event, COLLECTION).unwrap_err();
        assert!(!error.to_string().contains(selector));
    }
}

#[test]
fn structured_only_activity_and_policy_are_visible_with_exact_machine_evidence() {
    let mut event = fixture();
    event.record.content.normalized_body = None;
    event.record.content.structured_content = Some(json!({"answer":"structured-only", "count":7}));
    event.record.content.activity = Some(
        serde_json::from_value(json!({
            "revision":1, "provider_call_id":null,
            "facts":[{"kind":"command", "value":"recorded activity"}]
        }))
        .unwrap(),
    );
    event.record.validate_contract().unwrap();
    let value = serde_json::to_value(&event).unwrap();
    let text = render::tool_text(&value).unwrap();
    let terminal = capture(|ui| render::event(ui, "team", &event));
    for rendered in [&text, &terminal] {
        assert!(rendered.contains("Content policy: \"selected\""));
        assert!(rendered.contains("structured-only"));
        assert!(rendered.contains("recorded activity"));
        assert!(rendered.contains(&event.citation));
        assert!(rendered.contains(&event.session_citation));
    }
    let encoded = capture(|ui| render::json(ui, &event));
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
    for status in [
        CoreContentPolicyStatus::Omitted {
            reason: "not retained".into(),
        },
        CoreContentPolicyStatus::Redacted {
            reason: "private field".into(),
        },
    ] {
        // Core's redacted disposition retains a sanitized remainder; omitted
        // content has no retained body at all.
        event.record.content.normalized_body =
            matches!(&status, CoreContentPolicyStatus::Redacted { .. })
                .then(|| "[redacted] retained remainder".into());
        event.record.content.policy_status = status;
        event.record.content.structured_content = None;
        event.record.content.activity = None;
        event.record.validate_contract().unwrap();
        let expected = serde_json::to_string(&event.record.content.policy_status).unwrap();
        assert!(capture(|ui| render::event(ui, "team", &event)).contains(&expected));
        assert!(render::tool_text(&serde_json::to_value(&event).unwrap())
            .unwrap()
            .contains(&expected));
    }
}

#[test]
fn terminal_escapes_history_controls_but_json_and_mcp_preserve_them() {
    let mut event = fixture();
    let body = "literal\u{202e}command\u{2066}\u{2069}\u{1b}[31m · e\u{0301} 👩\u{200d}💻 שלום";
    event.record.content.normalized_body = Some(body.into());
    event.record.content.structured_content = Some(json!({"literal":"structured\u{2066}value"}));
    let terminal = capture(|ui| render::event(ui, "team", &event));
    for control in ['\u{202e}', '\u{2066}', '\u{2069}', '\u{1b}'] {
        assert!(!terminal.contains(control));
    }
    for visible in [
        "literal\\u{202e}command",
        "\\u{2066}",
        "\\u{2069}",
        "\\x1b[31m",
        "e\u{0301} 👩\u{200d}💻 שלום",
        "structured\\u{2066}value",
    ] {
        assert!(terminal.contains(visible), "missing {visible:?}");
    }
    let value = serde_json::to_value(&event).unwrap();
    let encoded = capture(|ui| render::json(ui, &event));
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
    assert!(render::tool_text(&value).unwrap().contains(body));
    let ordinary = fixture();
    let rendered = capture(|ui| render::event(ui, "team", &ordinary));
    assert_eq!(rendered.matches("ordinary retained evidence").count(), 1);
}

#[test]
fn search_snippets_are_escaped_only_for_terminal_and_exact_show_keeps_full_content() {
    let mut event = fixture();
    let snippet = "retained\u{202e} evidence\u{1b}[31m";
    event.record.content.normalized_body = Some(format!("{snippet}\nfull retained tail"));
    event.record.content.structured_content = Some(json!({"exact_only":"full structure"}));
    let response = SearchResponse {
        status: CollectionStatus {
            collection: COLLECTION.into(),
            stored_sequence: 1,
            searchable_sequence: 1,
            generation: None,
            reads_available: true,
            off_host_checkpoint: None,
        },
        results: vec![search_hit(&event, snippet, true)],
        complete: true,
        exhaustive: true,
    };
    let encoded = capture(|ui| render::json(ui, &response));
    let value: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value["results"][0]["snippet"], snippet);
    assert_eq!(value["results"][0]["snippet_truncated"], true);
    assert!(value["results"][0].get("record").is_none());
    let terminal = capture(|ui| render::search(ui, "team", &response));
    assert!(terminal.contains("retained\\u{202e} evidence\\x1b[31m"));
    assert!(!terminal.contains('\u{202e}') && !terminal.contains('\u{1b}'));
    let mcp = render::tool_text(&value).unwrap();
    assert!(mcp.contains(snippet));
    for text in [&terminal, &mcp] {
        assert!(text.contains("Content policy: \"selected\""));
        assert!(text.contains("Snippet truncated"));
        assert!(text.contains(&event.citation));
        assert!(text.contains(&event.session_citation));
        assert!(!text.contains("full retained tail"));
        assert!(!text.contains("full structure"));
    }
    let shown = capture(|ui| render::event(ui, "team", &event));
    assert!(shown.contains("full retained tail"));
    assert!(shown.contains("full structure"));
    assert!(!shown.contains("Snippet truncated"));
}

pub(super) fn search_hit(event: &HostedEvent, snippet: &str, truncated: bool) -> HostedSearchHit {
    HostedSearchHit {
        event_id: event.record.event_id.as_uuid(),
        session_id: event.record.session_id.as_uuid(),
        event_sequence: event.record.event_sequence,
        occurred_at_unix_ms: event.record.occurred_at_unix_ms,
        event_type: event.record.event_type.clone(),
        role: event.record.role.clone(),
        snippet: snippet.into(),
        snippet_truncated: truncated,
        content_status: event.record.content.policy_status.clone(),
        provenance: event.provenance.clone(),
        citation: event.citation.clone(),
        session_citation: event.session_citation.clone(),
        score: event.score,
    }
}

pub(super) fn fixture() -> HostedEvent {
    let source = SourceKey::derive(
        "custom",
        "synthetic_remote",
        "session",
        1,
        SourceAnchor::CatalogLineage([23; 32]),
    )
    .unwrap();
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("synthetic-session").unwrap())
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let event_key =
        NativeItemKey::native_id("message", TypedKey::utf8("synthetic-event").unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &event_key,
        subrecord_selector: None,
    })
    .unwrap();
    let record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
        "synthetic-v1",
        "ordinary retained evidence",
    )
    .unwrap();
    let citation = |kind, id| {
        Citation {
            collection: COLLECTION.into(),
            publication: "publication-one".into(),
            revision: "revision-one".into(),
            kind,
            id,
        }
        .encode()
        .unwrap()
    };
    HostedEvent {
        citation: citation(CitationKind::Event, event_id.as_uuid().to_string()),
        session_citation: citation(CitationKind::Session, session_id.as_uuid().to_string()),
        record,
        provenance: Provenance {
            collection: COLLECTION.into(),
            publication: "publication-one".into(),
            revision: "revision-one".into(),
            publisher: "publisher-one".into(),
            origin: "origin-one".into(),
            view: "view-one".into(),
            source,
        },
        score: None,
    }
}

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn capture(render: impl FnOnce(&mut Ui) -> anyhow::Result<()>) -> String {
    let output = Buffer::default();
    let mut ui = Ui::with_writers(
        output.clone(),
        crate::ui::RenderContext::canonical_human_measurement(),
        io::sink(),
        crate::ui::RenderContext::canonical_human_measurement(),
    );
    render(&mut ui).unwrap();
    ui.flush().unwrap();
    let bytes = output.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn search_notices_distinguish_query_bounds_from_indexing_backlog() {
    for (searchable, reads_available, complete, exhaustive, backlog, incomplete) in [
        (2, true, false, true, false, true),
        (2, true, true, false, false, true),
        (1, true, false, false, true, true),
        (2, true, true, true, false, false),
        (1, false, false, false, false, true),
    ] {
        let response = SearchResponse {
            status: CollectionStatus {
                collection: COLLECTION.into(),
                stored_sequence: 2,
                searchable_sequence: searchable,
                generation: None,
                reads_available,
                off_host_checkpoint: None,
            },
            results: vec![],
            complete,
            exhaustive,
        };
        let errors = Buffer::default();
        let mut ui = Ui::with_writers(
            io::sink(),
            crate::ui::RenderContext::canonical_human_measurement(),
            errors.clone(),
            crate::ui::RenderContext::canonical_human_measurement(),
        );
        render::search(&mut ui, "team", &response).unwrap();
        ui.flush().unwrap();
        let text = String::from_utf8(errors.0.lock().unwrap().clone()).unwrap();
        assert_eq!(text.contains("awaiting indexing"), backlog, "{response:?}");
        assert_eq!(
            text.contains("not an exhaustive history listing"),
            incomplete,
            "{response:?}"
        );
    }
}

#[test]
fn mcp_text_retains_evidence_and_exact_citations_for_text_only_clients() {
    let value = serde_json::json!({"events":[{
        "record":{"content":{"normalized_body":"retained marker\nsecond line"}},
        "citation":"immutable-event", "session_citation":"immutable-session"
    }],"next_cursor":"page-two"});
    let rendered = render::tool_text(&value).unwrap();
    assert!(rendered.contains("retained marker\nsecond line"));
    assert!(rendered.contains("citation: immutable-event"));
    assert!(rendered.contains("session_citation: immutable-session"));
    assert!(rendered.contains("page-two"));
}

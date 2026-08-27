use std::{
    io::{self, Write as _},
    sync::{Arc, Mutex},
};

use unicode_width::UnicodeWidthStr as _;

use super::*;
use crate::ui::{ColorMode, StreamKind, TestContext};

fn context(width: usize, color: ColorMode) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
}

fn assert_fits(document: &Document, context: &RenderContext) {
    let width = context.content_width().unwrap_or(1);
    for line in document.render_plain().lines() {
        assert!(line.width() <= width, "{line:?} exceeded {width} columns");
    }
}

fn strip_ansi(rendered: &str) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    stream.write_all(rendered.as_bytes()).unwrap();
    String::from_utf8(stream.into_inner()).unwrap()
}

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn test_ui() -> (Ui, SharedWriter, SharedWriter) {
    let stdout = SharedWriter::default();
    let stderr = SharedWriter::default();
    let ui = Ui::with_writers(
        stdout.clone(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        stderr.clone(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
    );
    (ui, stdout, stderr)
}

#[test]
fn docs_list_is_structured_and_responsive() {
    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_docs_list(&context);
        let rendered = document.render_plain();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.starts_with(&format!("{} embedded documentation topics", TOPICS.len())));
        assert!(rendered.contains("Topics\n"));
        assert!(rendered.contains("ctx docs search \"file path\""));
        assert_fits(&document, &context);
    }
}

#[test]
fn docs_machine_and_plain_branches_write_exact_selected_stdout_protocols() {
    let mut telemetry = DocsTelemetry::default();
    let (mut ui, stdout, stderr) = test_ui();

    list_docs(true, &mut telemetry, &mut ui).unwrap();
    let list: Value = serde_json::from_str(stdout.text().trim()).unwrap();
    assert_eq!(list["schema_version"], 1);
    assert_eq!(list["topics"].as_array().unwrap().len(), TOPICS.len());
    assert!(stderr.text().is_empty());

    let (mut ui, stdout, stderr) = test_ui();
    search_docs("search", 1, true, &mut DocsTelemetry::default(), &mut ui).unwrap();
    let search: Value = serde_json::from_str(stdout.text().trim()).unwrap();
    assert_eq!(search["query"], "search");
    assert_eq!(search["results"].as_array().unwrap().len(), 1);
    assert!(stderr.text().is_empty());

    let (mut ui, stdout, stderr) = test_ui();
    show_doc(
        DocsShowArgs {
            id: "docs".to_owned(),
            format: DocsFormat::Json,
            out: None,
        },
        &mut DocsTelemetry::default(),
        &mut ui,
    )
    .unwrap();
    let show: Value = serde_json::from_str(stdout.text().trim()).unwrap();
    assert_eq!(show["schema_version"], 1);
    assert_eq!(show["id"], "docs");
    assert_eq!(
        show["body"],
        TOPICS.iter().find(|topic| topic.id == "docs").unwrap().body
    );
    assert!(stderr.text().is_empty());

    let (mut ui, stdout, stderr) = test_ui();
    show_doc(
        DocsShowArgs {
            id: "docs".to_owned(),
            format: DocsFormat::Markdown,
            out: None,
        },
        &mut DocsTelemetry::default(),
        &mut ui,
    )
    .unwrap();
    assert!(stdout.text().starts_with("# Docs\n"));
    assert!(stdout.text().ends_with('\n'));
    assert!(stderr.text().is_empty());

    let (mut ui, stdout, stderr) = test_ui();
    man_docs(
        DocsManArgs {
            out: None,
            print: Some("ctx".to_owned()),
        },
        &mut ui,
        &Command::new("ctx"),
    )
    .unwrap();
    assert!(stdout.text().contains("ctx"));
    assert!(stdout.text().ends_with('\n'));
    assert!(stderr.text().is_empty());

    let directory = tempfile::tempdir().unwrap();
    let (mut ui, stdout, stderr) = test_ui();
    man_docs(
        DocsManArgs {
            out: Some(directory.path().join("man")),
            print: None,
        },
        &mut ui,
        &Command::new("ctx"),
    )
    .unwrap();
    assert!(directory.path().join("man/ctx.1").is_file());
    assert!(stdout.text().starts_with("✓ ctx man pages written\n"));
    assert!(stderr.text().is_empty());
}

#[test]
fn docs_search_success_is_outcome_first_and_actionable() {
    let topic = TOPICS.iter().find(|topic| topic.id == "search").unwrap();
    let context = context(48, ColorMode::Never);
    let document = render_docs_search(&context, "filters", &[(1_000, topic)]);
    let rendered = document.render_plain();
    assert!(rendered.starts_with("✓ 1 doc matched \"filters\"\n"));
    assert!(rendered.contains("Matches\n"));
    assert!(rendered.contains("Next\n  ctx docs show search\n"));
    assert_fits(&document, &context);
}

#[test]
fn event_queries_is_embedded_with_stable_search_tags() {
    let topic = TOPICS
        .iter()
        .find(|topic| topic.id == "event-queries")
        .unwrap();
    assert_eq!(topic.tags, ["events", "jsonl", "jq", "query"]);
    assert!(topic.body.contains("ctx list events"));
    assert_eq!(
        DocTopicId::from_known_id(topic.id).unwrap().as_str(),
        topic.id
    );
}

#[test]
fn stats_json_docs_track_schema_three_and_core_sqlite_five() {
    let topic = TOPICS
        .iter()
        .find(|topic| topic.id == "json-contracts")
        .unwrap();
    let stats = topic
        .body
        .split_once("## Stats\n")
        .unwrap()
        .1
        .split_once("\n## Sources")
        .unwrap()
        .0;

    assert!(stats.contains("`schema_version` is 3."));
    assert!(stats.contains("current SQLite\nschema version is 5"));
    assert!(stats.contains("`summary` contains exactly"));
    assert!(stats.contains("Each `by_operation` row contains exactly"));
    for field in [
        "`delivered_output_bytes`",
        "`delivered_context_bytes`",
        "`matched_normalized_session_bytes`",
        "`complete_context_eligible_calls`",
        "`unavailable_context_eligible_calls`",
    ] {
        assert!(stats.contains(field), "missing public field {field}");
    }
    for stale in [
        "`schema_version` is 2",
        "citation_count",
        "pro_blame",
        "target_type",
        "pro_outcome",
    ] {
        assert!(!stats.contains(stale), "stale Stats contract: {stale}");
    }
}

#[test]
fn docs_search_empty_state_neutralizes_query_controls() {
    let context = context(48, ColorMode::Never);
    let document = render_docs_search(&context, "missing\u{1b}[31m", &[]);
    let rendered = document.render_plain();
    assert!(rendered.starts_with("No docs matched \"missing\\x1b[31m\"\n"));
    assert!(rendered.contains("Next\n  ctx docs list\n"));
    assert!(!rendered.as_bytes().contains(&0x1b));
    assert_fits(&document, &context);
}

#[test]
fn docs_plain_output_matches_ansi_stripped_output() {
    let context = context(80, ColorMode::Always);
    let document = render_docs_list(&context);
    assert_eq!(
        strip_ansi(&document.render(&context)),
        document.render_plain()
    );
}

#[test]
fn unknown_topic_is_a_structured_diagnostic_without_literal_newline_escapes() {
    let context = context(80, ColorMode::Always);
    let document = render_unknown_doc_topic(&context, "cli");
    let plain = document.render_plain();
    assert!(
        plain.starts_with("✗ Unknown ctx docs topic: cli\n"),
        "{plain}"
    );
    assert!(
        plain.contains("Nearest topics\n  cli-reference\n"),
        "{plain}"
    );
    assert!(
        plain.contains("Next\n  ctx docs list\n  ctx docs search cli\n"),
        "{plain}"
    );
    assert_eq!(plain.lines().count(), 9, "{plain}");
    assert!(!plain.contains("\\n"), "{plain}");

    let styled = document.render(&context);
    assert!(styled.as_bytes().contains(&0x1b), "{styled:?}");
    assert_eq!(strip_ansi(&styled), plain);
}

#[test]
fn unknown_topic_neutralizes_user_control_characters() {
    let context = context(80, ColorMode::Never);
    let rendered = render_unknown_doc_topic(&context, "cli\u{1b}[31m").render_plain();
    assert!(rendered.contains("cli\\x1b[31m"), "{rendered}");
    assert!(!rendered.as_bytes().contains(&0x1b), "{rendered:?}");
}

use std::io::Write as _;

use clap::Parser as _;
use serde_json::{json, Value};
use unicode_width::UnicodeWidthStr as _;

use super::{
    canonical_human_output_bytes, render_locate_document, render_search_document,
    render_show_document, render_show_jsonl, render_show_markdown, render_show_text,
};
use crate::{
    cli::Cli,
    ui::{ColorMode, Document, RenderContext, StreamKind, TestContext, Token},
};

const SESSION_ID: &str = "01900000-0000-7000-8000-000000000001";
const EVENT_ID: &str = "01900001-0000-7000-8000-000000000002";
const SECOND_EVENT_ID: &str = "01900002-0000-7000-8000-000000000003";

fn context(width: usize, color: ColorMode) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
}

fn search_value() -> Value {
    json!({
        "query": "Unicode cache key",
        "results": [{
            "title": "codex user message",
            "snippet": "Fix the Unicode cache key regression in the parser.",
            "provider": "codex",
            "provider_session_id": "demo-unicode-session",
            "result_scope": "session",
            "rank": 0.84,
            "session_importance": 0.86,
            "more_matches_in_session": 2,
            "ctx_event_id": EVENT_ID,
            "ctx_session_id": SESSION_ID,
            "source_format": "codex_session_jsonl",
            "suggested_next_commands": [
                format!("ctx show event {EVENT_ID} --window 10"),
                format!("ctx show session {SESSION_ID}"),
                format!("ctx search 'Unicode cache key' --session {SESSION_ID}"),
            ],
        }],
        "result_window": {
            "limit": 10,
            "returned": 1,
            "more_available": false,
        },
        "truncation": {
            "candidate_pool_truncated": false,
        },
    })
}

fn empty_search_value() -> Value {
    json!({
        "query": "definitely-no-results-here",
        "results": [],
        "result_window": {
            "limit": 10,
            "returned": 0,
            "more_available": false,
        },
        "truncation": {
            "candidate_pool_truncated": false,
        },
    })
}

fn show_value() -> Value {
    json!({
        "target": "session",
        "ctx_session_id": SESSION_ID,
        "provider": "codex",
        "provider_session_id": "demo-unicode-session",
        "mode": "lite",
        "content_policy": "complete",
        "format": "text",
        "events": [
            {
                "ctx_event_id": EVENT_ID,
                "ctx_session_id": SESSION_ID,
                "provider": "codex",
                "provider_session_id": "demo-unicode-session",
                "role": "user",
                "event_type": "message",
                "occurred_at": "2026-07-30T12:00:00.000Z",
                "text": "Fix the Unicode cache key regression.\nKeep source bytes exact.",
            },
            {
                "ctx_event_id": SECOND_EVENT_ID,
                "ctx_session_id": SESSION_ID,
                "provider": "codex",
                "provider_session_id": "demo-unicode-session",
                "role": "assistant",
                "event_type": "message",
                "occurred_at": "2026-07-30T12:01:00.000Z",
                "text": "Done.",
            },
        ],
    })
}

fn locate_session_value() -> Value {
    json!({
        "target": "session",
        "ctx_session_id": SESSION_ID,
        "provider": "codex",
        "provider_session_id": "demo-unicode-session",
        "source": {
            "path": "/tmp/ctx/history/demo-unicode-session.jsonl",
            "source_format": "codex_session_jsonl_tree",
            "exists": true,
        },
        "resume": {
            "available": true,
            "command": "codex resume demo-unicode-session",
        },
    })
}

fn locate_event_value() -> Value {
    json!({
        "target": "event",
        "ctx_event_id": EVENT_ID,
        "ctx_session_id": SESSION_ID,
        "provider": "codex",
        "provider_session_id": "demo-unicode-session",
        "event_type": "message",
        "role": "assistant",
        "source": {
            "path": "/tmp/ctx/history/demo-unicode-session.jsonl",
            "source_format": "codex_session_jsonl_tree",
            "exists": false,
        },
        "source_record": {
            "kind": "jsonl",
            "byte_offset": 128,
            "byte_length": 64,
            "ordinal": 7,
        },
        "complete_content": {
            "locator_available": true,
            "available": false,
        },
        "resume": {
            "available": true,
            "command": "codex resume demo-unicode-session",
        },
    })
}

fn assert_fits(document: &Document, context: &RenderContext) {
    let available = context.content_width().unwrap_or(1);
    let plain = document.render_plain();
    for (line, rendered) in document.lines().iter().zip(plain.lines()) {
        let preserves_unbroken_command_arg = line
            .spans()
            .iter()
            .filter(|span| span.token() == Token::Command)
            .flat_map(|span| span.content().split_whitespace())
            .any(|word| word.width() > available);
        assert!(
            rendered.width() <= available || preserves_unbroken_command_arg,
            "{rendered:?} is {} columns in a {available}-column content area",
            rendered.width()
        );
    }
}

fn assert_control_safe(rendered: &str) {
    for character in rendered.chars() {
        assert!(
            character == '\n'
                || (character >= ' '
                    && character != '\u{7f}'
                    && !('\u{80}'..='\u{9f}').contains(&character)),
            "unsafe terminal control {character:?} survived in {rendered:?}"
        );
    }
}

fn assert_value_survives_layout(rendered: &str, value: &str) {
    let compact = rendered
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(
        compact.contains(value),
        "{value:?} was not retained across renderer-controlled lines in {rendered:?}"
    );
}

fn strip_ansi(rendered: &str) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    stream.write_all(rendered.as_bytes()).unwrap();
    String::from_utf8(stream.into_inner()).unwrap()
}

#[test]
fn primary_renderers_match_reference_goldens_at_80_columns() {
    let context = context(80, ColorMode::Never);
    assert_eq!(
        render_search_document(&search_value(), false, &context).render_plain(),
        include_str!("goldens/search.txt")
    );
    assert_eq!(
        render_show_document(&show_value(), &context).render_plain(),
        include_str!("goldens/show.txt")
    );
    assert_eq!(
        render_locate_document(&locate_event_value(), &context).render_plain(),
        include_str!("goldens/locate.txt")
    );
}

#[test]
fn no_results_is_an_actionable_empty_state() {
    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_search_document(&empty_search_value(), false, &context);
        let rendered = document.render_plain();
        if width == 80 {
            assert_eq!(rendered, include_str!("goldens/search_empty.txt"));
        }
        assert!(rendered.starts_with("No results for"));
        assert!(rendered.contains("Try broader terms\n"));
        assert!(rendered.contains("ctx search \"<term>\""));
        assert_fits(&document, &context);
    }
}

#[test]
fn empty_search_action_is_a_valid_positional_query() {
    let rendered =
        render_search_document(&empty_search_value(), false, &context(80, ColorMode::Never))
            .render_plain();
    assert!(rendered.contains("ctx search \"<term>\""));
    assert!(!rendered.contains("--term"));
    Cli::try_parse_from(["ctx", "search", "<term>"])
        .expect("empty-state action must be a valid positional search invocation");
}

#[test]
fn more_available_footer_has_exact_contract_bytes_and_no_guidance() {
    let context = context(80, ColorMode::Never);
    let value = search_value();
    let without_more = render_search_document(&value, false, &context).render_plain();
    let mut with_more = value;
    with_more["result_window"]["more_available"] = json!(true);
    let rendered = render_search_document(&with_more, false, &context).render_plain();

    assert_eq!(
        rendered.as_bytes(),
        format!("{without_more}\nMore results available.\n").as_bytes()
    );
    assert!(!rendered.contains("Refine"));
    assert!(!rendered.contains("higher --limit"));
}

#[test]
fn narrow_actions_preserve_full_ids_inside_copyable_commands() {
    let document = render_search_document(&search_value(), false, &context(32, ColorMode::Never));
    let commands = document
        .lines()
        .iter()
        .flat_map(|line| line.spans())
        .filter(|span| span.token() == Token::Command)
        .map(|span| span.content())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(commands.contains(EVENT_ID), "{commands}");
}

#[test]
fn primary_verbose_narrow_and_long_id_cases_fit_reference_widths() {
    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let documents = [
            render_search_document(&search_value(), false, &context),
            render_search_document(&search_value(), true, &context),
            render_show_document(&show_value(), &context),
            render_locate_document(&locate_session_value(), &context),
            render_locate_document(&locate_event_value(), &context),
        ];
        for document in documents {
            assert_fits(&document, &context);
            assert_control_safe(&document.render_plain());
        }

        let verbose = render_search_document(&search_value(), true, &context).render_plain();
        assert_value_survives_layout(&verbose, EVENT_ID);
        assert_value_survives_layout(&verbose, SESSION_ID);

        let shown = render_show_document(&show_value(), &context).render_plain();
        assert_value_survives_layout(&shown, EVENT_ID);
        assert_value_survives_layout(&shown, SECOND_EVENT_ID);
        assert_value_survives_layout(&shown, SESSION_ID);

        let located = render_locate_document(&locate_event_value(), &context).render_plain();
        assert_value_survives_layout(&located, EVENT_ID);
        assert_value_survives_layout(&located, SESSION_ID);
        assert!(!located.contains("source_exists"));
        assert!(!located.contains("locator_available"));
        assert!(!located.contains("available: false"));
        assert!(!located.contains("source_record_"));
    }
}

#[test]
fn show_splits_real_newlines_then_neutralizes_each_content_line() {
    let attack = concat!(
        "first line\n",
        "\u{1b}[31mred\u{1b}[0m\rrewrite",
        "\u{0000}\t\u{007f}\u{0085}\u{009b}2J\n",
        "\nlast line"
    );
    let mut value = show_value();
    value["events"][0]["text"] = json!(attack);

    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_show_document(&value, &context);
        let rendered = document.render_plain();
        assert_fits(&document, &context);
        assert_control_safe(&rendered);
        assert!(rendered.contains("\\x1b[31mred\\x1b[0m"));
        assert!(rendered.contains("\\rrewrite"));
        assert!(rendered.contains("\\u{0000}"));
        assert!(rendered.contains("\\t"));
        assert!(rendered.contains("\\u{007f}"));
        assert!(rendered.contains("\\u{0085}"));
        assert!(rendered.contains("\\u{009b}2J"));
        assert!(!rendered.contains("first line\\n"));
    }

    let wide = render_show_document(&value, &context(120, ColorMode::Never)).render_plain();
    assert!(wide.contains("   first line\n   \\x1b[31mred"));
    assert!(wide.contains("\\u{009b}2J\n   \n   last line\n"));
}

#[test]
fn search_candidate_warning_and_more_available_footer_are_actionable() {
    let mut value = search_value();
    value["truncation"]["candidate_pool_truncated"] = json!(true);
    value["result_window"]["more_available"] = json!(true);

    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_search_document(&value, false, &context);
        let rendered = document.render_plain();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(rendered.contains("Warning\n"));
        assert!(normalized.contains("Refine the query"));
        assert!(normalized.contains("provider, workspace, file, or session filter"));
        assert!(rendered.ends_with("More results available.\n"));
        assert_fits(&document, &context);
    }
}

#[test]
fn commands_are_semantically_styled_and_locate_omits_routine_booleans() {
    let context = context(80, ColorMode::Always);
    for document in [
        render_search_document(&search_value(), false, &context),
        render_search_document(&empty_search_value(), false, &context),
        render_locate_document(&locate_session_value(), &context),
        render_locate_document(&locate_event_value(), &context),
    ] {
        assert!(
            document
                .lines()
                .iter()
                .flat_map(|line| line.spans())
                .any(|span| span.token() == Token::Command),
            "renderer omitted a semantic command span"
        );
    }

    let available = render_locate_document(&locate_session_value(), &context).render_plain();
    assert!(!available.contains("exists"));
    assert!(!available.contains("available"));
    assert!(!available.contains("true"));
    let missing = render_locate_document(&locate_event_value(), &context).render_plain();
    assert!(missing.contains("Status   missing\n"));
    assert!(!missing.contains("false"));
}

#[test]
fn styled_output_strips_to_plain_and_canonical_bytes_ignore_color() {
    for width in [32, 48, 80, 120] {
        let styled = context(width, ColorMode::Always);
        let plain = context(width, ColorMode::Never);
        let styled_documents = [
            render_search_document(&search_value(), false, &styled),
            render_search_document(&search_value(), true, &styled),
            render_search_document(&empty_search_value(), false, &styled),
            render_show_document(&show_value(), &styled),
            render_locate_document(&locate_session_value(), &styled),
            render_locate_document(&locate_event_value(), &styled),
        ];
        let plain_documents = [
            render_search_document(&search_value(), false, &plain),
            render_search_document(&search_value(), true, &plain),
            render_search_document(&empty_search_value(), false, &plain),
            render_show_document(&show_value(), &plain),
            render_locate_document(&locate_session_value(), &plain),
            render_locate_document(&locate_event_value(), &plain),
        ];

        for (styled_document, plain_document) in styled_documents.into_iter().zip(plain_documents) {
            let styled_output = styled_document.render(&styled);
            assert!(styled_output.contains("\u{1b}["));
            assert_eq!(strip_ansi(&styled_output), plain_document.render_plain());
            assert_eq!(
                canonical_human_output_bytes(&styled_document),
                plain_document.render_plain().len()
            );
        }
    }
}

#[test]
fn compatibility_string_and_raw_format_renderers_keep_their_existing_bytes() {
    let value = show_value();
    assert_eq!(
        render_show_text(&value),
        concat!(
            "ctx_session_id: 01900000-0000-7000-8000-000000000001\n",
            "provider: codex\n",
            "provider_session_id: demo-unicode-session\n",
            "mode: lite\n",
            "content: complete\n",
            "format: text\n\n",
            "[2026-07-30T12:00:00.000Z] user message ",
            "01900001-0000-7000-8000-000000000002\n",
            "Fix the Unicode cache key regression.\n",
            "Keep source bytes exact.\n\n",
            "[2026-07-30T12:01:00.000Z] assistant message ",
            "01900002-0000-7000-8000-000000000003\n",
            "Done.\n\n",
        )
    );
    let jsonl = render_show_jsonl(&value).unwrap();
    let markdown = render_show_markdown(&value);
    assert!(!jsonl.contains('\u{1b}'));
    assert!(!markdown.contains('\u{1b}'));
    assert!(jsonl.contains("Fix the Unicode cache key regression."));
    assert!(markdown.contains("Keep source bytes exact."));
}

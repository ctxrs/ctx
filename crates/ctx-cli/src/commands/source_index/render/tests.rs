use std::io::Write as _;

use clap::Parser as _;
use ctx_history_core::MAX_CORE_CONTENT_BYTES;
use serde_json::{json, Value};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use super::{
    render_search_document, render_show_document, render_show_jsonl, render_show_markdown,
    render_show_text, search_snippet_fragment, SEARCH_SNIPPET_MAX_BYTES, SEARCH_SNIPPET_MAX_CHARS,
};
use crate::{
    cli::Cli,
    commands::mcp_tool_call::{MCP_TOOL_CALL_DISPLAY_MAX_CHARS, MCP_TOOL_CALL_JSON_GUIDANCE},
    ui::{
        canonical_human_output_bytes, is_copyable_atom, ColorMode, Document, RenderContext,
        StreamKind, TestContext, Token,
    },
};

const SESSION_ID: &str = "01900000-0000-7000-8000-000000000001";
const PARENT_SESSION_ID: &str = "01900000-0000-7000-8000-000000000010";
const ROOT_SESSION_ID: &str = "01900000-0000-7000-8000-000000000020";
const EVENT_ID: &str = "01900001-0000-7000-8000-000000000002";
const SECOND_EVENT_ID: &str = "01900002-0000-7000-8000-000000000003";

#[test]
fn search_snippet_centers_the_actual_match_after_character_4000() {
    let body = format!("{}NeEdLe{}", "a".repeat(4_500), "z".repeat(4_500));

    let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

    assert!(truncated);
    assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);
    let match_offset = snippet.find("NeEdLe").expect("matched term stays visible");
    assert_eq!(snippet[..match_offset].graphemes(true).count(), 157);
}

#[test]
fn search_snippet_prefers_late_unique_identifier_over_early_generic_term() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let body = format!("commit {} {oid} tail", "x".repeat(4_500));
    let query = format!("commit {oid}");

    let (snippet, truncated) = search_snippet_fragment(&body, &[&query]);

    assert!(truncated);
    assert!(snippet.contains(oid));
    assert!(!snippet.starts_with("commit"));
}

#[test]
fn search_snippet_keeps_combining_and_emoji_graphemes_intact() {
    let combining = "e\u{301}";
    let family = "👨‍👩‍👧‍👦";
    let body = format!("{}目标{}", combining.repeat(400), family.repeat(400));

    let (snippet, truncated) = search_snippet_fragment(&body, &["目标"]);
    let graphemes = snippet.graphemes(true).collect::<Vec<_>>();

    assert!(truncated);
    assert_eq!(graphemes.len(), SEARCH_SNIPPET_MAX_CHARS);
    assert_eq!(graphemes.first().copied(), Some(combining));
    assert_eq!(graphemes.last().copied(), Some(family));
    assert!(snippet.contains("目标"));
}

#[test]
fn search_snippet_byte_bounds_pathological_large_grapheme_clusters_around_the_query() {
    let oversized_cluster = format!("x{}", "\u{301}".repeat(SEARCH_SNIPPET_MAX_BYTES));
    let body = format!("{oversized_cluster}needle{oversized_cluster}");

    let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

    assert!(truncated);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(snippet.contains("needle"));
    let start = body.find(&snippet).expect("snippet remains a body window");
    let end = start + snippet.len();
    assert!(body
        .grapheme_indices(true)
        .any(|(offset, _)| offset == start));
    assert!(end == body.len() || body.grapheme_indices(true).any(|(offset, _)| offset == end));
    assert_eq!(snippet.graphemes(true).next(), Some("n"));
    assert_eq!(snippet.graphemes(true).next_back(), Some("e"));
    assert!(!snippet.starts_with('\u{301}'));
    assert!(!snippet.ends_with('\u{301}'));

    let first = search_snippet_fragment(&oversized_cluster, &["x"]);
    let second = search_snippet_fragment(&oversized_cluster, &["x"]);
    assert_eq!(first, second);
    assert_eq!(first, (String::new(), true));

    let metadata_only = search_snippet_fragment(&oversized_cluster, &[]);
    assert_eq!(metadata_only, (String::new(), true));
}

#[test]
fn search_snippet_handles_start_end_and_no_match_fallback_truthfully() {
    let at_start = format!("needle{}", "x".repeat(500));
    let (snippet, truncated) = search_snippet_fragment(&at_start, &["needle"]);
    assert!(truncated);
    assert!(snippet.starts_with("needle"));
    assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);

    let at_end = format!("{}needle", "x".repeat(500));
    let (snippet, truncated) = search_snippet_fragment(&at_end, &["needle"]);
    assert!(truncated);
    assert!(snippet.ends_with("needle"));
    assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);

    let no_match = "x".repeat(500);
    let (snippet, truncated) = search_snippet_fragment(&no_match, &["absent"]);
    assert!(truncated);
    assert_eq!(snippet, "x".repeat(SEARCH_SNIPPET_MAX_CHARS));

    let short = "short body";
    let (snippet, truncated) = search_snippet_fragment(short, &[]);
    assert!(!truncated);
    assert_eq!(snippet, short);

    let crlf = "\r\n".repeat(500);
    let (snippet, truncated) = search_snippet_fragment(&crlf, &[]);
    assert!(truncated);
    assert_eq!(snippet, "\r\n".repeat(SEARCH_SNIPPET_MAX_CHARS));
    assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);
}

#[test]
fn search_snippet_handles_a_maximum_valid_core_body_without_offset_vectors() {
    let needle = "NeEdLe";
    let mut body = "x".repeat(MAX_CORE_CONTENT_BYTES - needle.len());
    body.push_str(needle);

    let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

    assert!(truncated);
    assert_eq!(snippet, format!("{}{}", "x".repeat(314), needle));
    assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);
}

#[test]
fn search_snippet_keeps_exact_boundaries_for_a_maximum_valid_unicode_core_body() {
    let combining = "e\u{301}";
    let marker = "İSTANBUL";
    let prefix_bytes = MAX_CORE_CONTENT_BYTES - marker.len();
    let prefix_graphemes = prefix_bytes / combining.len();
    let ascii_remainder = prefix_bytes % combining.len();
    let mut body = combining.repeat(prefix_graphemes);
    body.push_str(&"x".repeat(ascii_remainder));
    body.push_str(marker);
    assert_eq!(body.len(), MAX_CORE_CONTENT_BYTES);

    let (snippet, truncated) = search_snippet_fragment(&body, &["i\u{307}stanbul"]);

    let marker_graphemes = marker.graphemes(true).count();
    let expected_combining = SEARCH_SNIPPET_MAX_CHARS - marker_graphemes - ascii_remainder;
    let expected = format!(
        "{}{}{}",
        combining.repeat(expected_combining),
        "x".repeat(ascii_remainder),
        marker
    );
    assert!(truncated);
    assert_eq!(snippet, expected);
    assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);
}

#[test]
fn search_snippet_preserves_exact_unicode_casefold_and_grapheme_boundaries() {
    let combining = "e\u{301}";
    let family = "👨‍👩‍👧‍👦";
    let body = format!(
        "{}İSTANBUL-Σ-{}-tail",
        combining.repeat(360),
        family.repeat(360)
    );

    let (expanded_case, expanded_truncated) = search_snippet_fragment(&body, &["i\u{307}stanbul"]);
    let (inside_grapheme, inside_truncated) = search_snippet_fragment(&body, &["\u{301}"]);

    assert!(expanded_truncated);
    assert!(inside_truncated);
    assert_eq!(
        expanded_case.graphemes(true).count(),
        SEARCH_SNIPPET_MAX_CHARS
    );
    assert_eq!(
        inside_grapheme.graphemes(true).count(),
        SEARCH_SNIPPET_MAX_CHARS
    );
    assert!(expanded_case.contains("İSTANBUL"));
    assert_eq!(inside_grapheme.graphemes(true).next(), Some(combining));
    assert!(!inside_grapheme.starts_with('\u{301}'));
}

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
            "rank": 1,
            "retrieval_score": 0.86,
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
        "format": "text",
        "session": {
            "ctx_session_id": SESSION_ID,
            "parent_ctx_session_id": null,
            "root_ctx_session_id": SESSION_ID,
        },
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

fn session_show_value(parent: Option<&str>, root: &str) -> Value {
    let mut value = show_value();
    value["session"]["parent_ctx_session_id"] = parent.map_or(Value::Null, |parent| json!(parent));
    value["session"]["root_ctx_session_id"] = json!(root);
    value
}

fn event_show_value(parent: Option<&str>, root: &str) -> Value {
    let mut value = show_value();
    value["target"] = json!("event");
    value["ctx_event_id"] = json!(EVENT_ID);
    let mut selected = value["events"][0].clone();
    selected["parent_ctx_session_id"] = parent.map_or(Value::Null, |parent| json!(parent));
    selected["root_ctx_session_id"] = json!(root);
    value["event"] = selected;
    value
}

fn lineage_show_values(parent: Option<&str>, root: &str) -> [Value; 2] {
    [
        session_show_value(parent, root),
        event_show_value(parent, root),
    ]
}

fn rendered_field_value(rendered: &str, label: &str) -> Option<String> {
    let prefix = format!("{label:<16}");
    rendered.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn assert_fits(document: &Document, context: &RenderContext) {
    let available = context.content_width().unwrap_or(1);
    let plain = document.render_plain();
    for (line, rendered) in document.lines().iter().zip(plain.lines()) {
        let preserves_unbroken_copyable_atom = line
            .spans()
            .iter()
            .flat_map(|span| span.content().split_whitespace())
            .any(|word| is_copyable_atom(word) && word.width() > available);
        assert!(
            rendered.width() <= available || preserves_unbroken_copyable_atom,
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
    assert!(
        rendered.contains(value),
        "{value:?} was split or removed by renderer-controlled lines in {rendered:?}"
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
}

#[test]
fn nested_lineage_is_visible_for_session_and_event_show() {
    for value in lineage_show_values(Some(PARENT_SESSION_ID), ROOT_SESSION_ID) {
        let rendered = render_show_document(&value, &context(80, ColorMode::Never)).render_plain();

        assert_eq!(
            rendered_field_value(&rendered, "Session").as_deref(),
            Some(SESSION_ID)
        );
        assert_eq!(
            rendered_field_value(&rendered, "Parent").as_deref(),
            Some(PARENT_SESSION_ID)
        );
        assert_eq!(
            rendered_field_value(&rendered, "Root").as_deref(),
            Some(ROOT_SESSION_ID)
        );
        assert!(rendered_field_value(&rendered, "Parent / root").is_none());
        for id in [SESSION_ID, PARENT_SESSION_ID, ROOT_SESSION_ID] {
            assert_eq!(rendered.matches(id).count(), 1, "{rendered}");
        }
    }
}

#[test]
fn one_level_lineage_collapses_repeated_parent_and_root_identity() {
    for value in lineage_show_values(Some(ROOT_SESSION_ID), ROOT_SESSION_ID) {
        let rendered = render_show_document(&value, &context(80, ColorMode::Never)).render_plain();

        assert_eq!(
            rendered_field_value(&rendered, "Parent / root").as_deref(),
            Some(ROOT_SESSION_ID)
        );
        assert!(rendered_field_value(&rendered, "Parent").is_none());
        assert!(rendered_field_value(&rendered, "Root").is_none());
        assert_eq!(rendered.matches(ROOT_SESSION_ID).count(), 1, "{rendered}");
    }
}

#[test]
fn primary_lineage_omits_redundant_hierarchy_fields() {
    for value in lineage_show_values(None, SESSION_ID) {
        let rendered = render_show_document(&value, &context(80, ColorMode::Never)).render_plain();

        assert_eq!(
            rendered_field_value(&rendered, "Session").as_deref(),
            Some(SESSION_ID)
        );
        assert!(rendered_field_value(&rendered, "Parent / root").is_none());
        assert!(rendered_field_value(&rendered, "Parent").is_none());
        assert!(rendered_field_value(&rendered, "Root").is_none());
        assert_eq!(rendered.matches(SESSION_ID).count(), 1, "{rendered}");
    }
}

#[test]
fn narrow_lineage_keeps_every_session_id_copyable_and_complete() {
    for value in lineage_show_values(Some(PARENT_SESSION_ID), ROOT_SESSION_ID) {
        let context = context(24, ColorMode::Never);
        let document = render_show_document(&value, &context);
        let rendered = document.render_plain();
        assert_fits(&document, &context);

        let references = document
            .lines()
            .iter()
            .flat_map(|line| line.spans())
            .filter(|span| span.token() == Token::Reference)
            .map(|span| span.content())
            .collect::<Vec<_>>();
        for id in [SESSION_ID, PARENT_SESSION_ID, ROOT_SESSION_ID] {
            assert_value_survives_layout(&rendered, id);
            assert!(references.contains(&id), "{id} was not one copyable span");
        }
    }
}

#[test]
fn human_lineage_keeps_compatibility_renderer_bytes_unchanged() {
    let fixtures = [
        (
            session_show_value(None, SESSION_ID),
            session_show_value(Some(PARENT_SESSION_ID), ROOT_SESSION_ID),
        ),
        (
            event_show_value(None, SESSION_ID),
            event_show_value(Some(PARENT_SESSION_ID), ROOT_SESSION_ID),
        ),
    ];

    for (ordinary, nested) in fixtures {
        assert_eq!(render_show_text(&nested), render_show_text(&ordinary));
        assert_eq!(
            render_show_markdown(&nested),
            render_show_markdown(&ordinary)
        );
        assert_eq!(
            render_show_jsonl(&nested).unwrap(),
            render_show_jsonl(&ordinary).unwrap()
        );
    }
}

#[test]
fn human_search_ranks_non_monotonic_scores_by_shaped_result_order() {
    let mut value = search_value();
    let mut first = value["results"][0].clone();
    first["rank"] = json!(1);
    first["retrieval_score"] = json!(0.25);
    first["session_importance"] = json!(0.25);
    let mut second = first.clone();
    second["rank"] = json!(2);
    second["retrieval_score"] = json!(9.5);
    second["session_importance"] = json!(9.5);
    second["ctx_event_id"] = json!(SECOND_EVENT_ID);
    value["results"] = json!([first, second]);
    value["result_window"]["returned"] = json!(2);

    let rendered =
        render_search_document(&value, false, &context(80, ColorMode::Never)).render_plain();
    let match_lines = rendered
        .lines()
        .filter(|line| line.trim_start().starts_with("Match"))
        .collect::<Vec<_>>();
    assert_eq!(match_lines.len(), 2, "{rendered}");
    assert!(match_lines[0].contains("#1"), "{rendered}");
    assert!(match_lines[1].contains("#2"), "{rendered}");
    assert!(!match_lines
        .iter()
        .any(|line| line.contains("0.25") || line.contains("9.50")));
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
fn primary_verbose_narrow_and_long_id_cases_preserve_reference_atoms() {
    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let documents = [
            render_search_document(&search_value(), false, &context),
            render_search_document(&search_value(), true, &context),
            render_show_document(&show_value(), &context),
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
fn commands_are_semantically_styled() {
    let context = context(80, ColorMode::Always);
    for document in [
        render_search_document(&search_value(), false, &context),
        render_search_document(&empty_search_value(), false, &context),
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
        ];
        let plain_documents = [
            render_search_document(&search_value(), false, &plain),
            render_search_document(&search_value(), true, &plain),
            render_search_document(&empty_search_value(), false, &plain),
            render_show_document(&show_value(), &plain),
        ];

        for (styled_document, plain_document) in styled_documents.into_iter().zip(plain_documents) {
            let styled_output = styled_document.render(&styled);
            assert!(styled_output.contains("\u{1b}["));
            assert_eq!(strip_ansi(&styled_output), plain_document.render_plain());
        }
    }
}

#[test]
fn canonical_human_bytes_ignore_live_width_color_and_pipe_capabilities() {
    let expected = canonical_human_output_bytes(|measurement| {
        render_search_document(&search_value(), false, measurement)
    });
    let mut live_lengths = Vec::new();
    for live in [
        context(32, ColorMode::Always),
        context(80, ColorMode::Never),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
    ] {
        let live_document = render_search_document(&search_value(), false, &live);
        live_lengths.push(live_document.render_plain().len());
        assert_eq!(
            canonical_human_output_bytes(|measurement| {
                render_search_document(&search_value(), false, measurement)
            }),
            expected
        );
    }
    live_lengths.sort_unstable();
    live_lengths.dedup();
    assert!(
        live_lengths.len() > 1,
        "fixture must prove live wrapping changes rendered byte counts"
    );
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

#[test]
fn mcp_attribution_is_machine_exact_and_human_safe_with_visible_truncation() {
    let exact_server = format!(
        "literal\\n\n# heading\u{202e}\u{1b}[2J|`[]{}",
        "x".repeat(MCP_TOOL_CALL_DISPLAY_MAX_CHARS)
    );
    let exact_tool = "tool\\literal\t*#_{}<>";
    let attribution = json!({
        "server": exact_server,
        "tool": exact_tool,
    });
    let mut session = show_value();
    session["events"][0]["mcp_tool_call"] = attribution.clone();
    let mut event = event_show_value(None, SESSION_ID);
    event["events"][0]["mcp_tool_call"] = attribution.clone();

    for value in [&session, &event] {
        let jsonl = render_show_jsonl(value).unwrap();
        let first: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        let machine_event = first.get("event").unwrap_or(&first);
        assert_eq!(machine_event["mcp_tool_call"], attribution);
        assert!(machine_event["mcp_tool_call"]["server"]
            .as_str()
            .unwrap()
            .contains('\u{202e}'));
        assert!(!jsonl.contains("display truncated"));
        let lines = jsonl.lines().collect::<Vec<_>>();
        let absent: Value = serde_json::from_str(lines[1]).unwrap();
        let absent_event = absent.get("event").unwrap_or(&absent);
        assert!(absent_event.get("mcp_tool_call").is_none());
    }

    let terminal = render_show_document(&session, &context(200, ColorMode::Never)).render_plain();
    let text = render_show_text(&session);
    for rendered in [&terminal, &text] {
        assert_control_safe(rendered);
        assert!(!rendered.contains('\u{202e}'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("literal\\\\n\\n#"), "{rendered:?}");
        assert!(
            rendered.contains("heading\\u{202e}\\x1b[2J"),
            "{rendered:?}"
        );
        assert!(rendered.contains("… [display truncated]"));
        assert!(rendered.contains(MCP_TOOL_CALL_JSON_GUIDANCE));
    }

    let markdown = render_show_markdown(&session);
    assert_control_safe(&markdown);
    assert!(!markdown.contains('\u{202e}'));
    assert!(!markdown.contains('\u{1b}'));
    assert!(!markdown.contains("\n# heading"));
    assert!(markdown.contains("- MCP server:"));
    assert!(markdown.contains("\\# heading"));
    assert!(markdown.contains("\\|\\`\\[\\]"));
    assert!(markdown.contains("… \\[display truncated\\]"));
    assert!(markdown.contains(MCP_TOOL_CALL_JSON_GUIDANCE));
}

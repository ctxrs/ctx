use ctx_history_core::MAX_CORE_CONTENT_BYTES;
use serde_json::{json, Value};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use super::{
    markdown_code_span, render_locate_document, render_search_document, render_show_document,
    render_show_jsonl, render_show_markdown, render_show_text, safe_activity_json,
    search_snippet_fragment, SEARCH_SNIPPET_MAX_BYTES, SEARCH_SNIPPET_MAX_CHARS,
};
use crate::ui::{
    canonical_human_output_bytes, is_copyable_atom, ColorMode, Document, RenderContext, StreamKind,
    TestContext, Token,
};

const SESSION_ID: &str = "01900000-0000-7000-8000-000000000001";
const SESSION_REF: &str = "01900000";
const PARENT_SESSION_ID: &str = "01900010-0000-7000-8000-000000000010";
const ROOT_SESSION_ID: &str = "01900020-0000-7000-8000-000000000020";
const PARENT_SESSION_REF: &str = "019000100";
const ROOT_SESSION_REF: &str = "019000101";
const EVENT_ID: &str = "01900001-0000-7000-8000-000000000002";
const EVENT_REF: &str = "01900001";
const SECOND_EVENT_ID: &str = "01900002-0000-7000-8000-000000000003";

#[test]
fn search_snippet_centers_the_actual_match_after_character_4000() {
    let body = format!("{}NeEdLe {}", "alpha ".repeat(750), "omega ".repeat(750));

    let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

    assert!(truncated);
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(snippet.contains("NeEdLe"));
    assert!(!snippet.starts_with("lpha"));
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
    // Keep the queried term analyzer-distinct from the `x` that anchors each
    // oversized combining cluster.
    let body = format!("{oversized_cluster} needle {oversized_cluster}");

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
    let at_start = format!("needle {}", "x".repeat(500));
    let (snippet, truncated) = search_snippet_fragment(&at_start, &["needle"]);
    assert!(truncated);
    assert!(snippet.starts_with("needle"));
    assert_eq!(snippet.graphemes(true).count(), SEARCH_SNIPPET_MAX_CHARS);

    let at_end = format!("{} needle", "x".repeat(500));
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
    let suffix = " NeEdLe";
    let mut body = "x".repeat(MAX_CORE_CONTENT_BYTES - suffix.len());
    body.push_str(suffix);

    let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);

    assert!(truncated);
    assert!(!snippet.is_empty());
    assert!(snippet.ends_with("NeEdLe"));
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
}

#[test]
fn search_snippet_keeps_exact_boundaries_for_a_maximum_valid_unicode_core_body() {
    let combining = "e\u{301}";
    let marker = "İSTANBUL";
    let prefix_bytes = MAX_CORE_CONTENT_BYTES - marker.len() - 1;
    let prefix_graphemes = prefix_bytes / combining.len();
    let ascii_remainder = prefix_bytes % combining.len();
    let mut body = combining.repeat(prefix_graphemes);
    body.push_str(&"x".repeat(ascii_remainder));
    body.push(' ');
    body.push_str(marker);
    assert_eq!(body.len(), MAX_CORE_CONTENT_BYTES);

    let (snippet, truncated) = search_snippet_fragment(&body, &[marker]);

    assert!(truncated);
    assert!(snippet.ends_with(marker));
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
    assert_ne!(snippet.graphemes(true).next(), Some("\u{301}"));
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

    let (expanded_case, expanded_truncated) = search_snippet_fragment(&body, &["İSTANBUL"]);
    let (inside_grapheme, inside_truncated) = search_snippet_fragment(&body, &["\u{301}"]);

    assert!(expanded_truncated);
    assert!(inside_truncated);
    assert!(expanded_case.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
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

fn context_with_unicode(width: usize, unicode: bool) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).unicode(unicode))
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
            "event_seq": 17,
            "timestamp": "2026-07-30T12:00:00.123Z",
            "source_format": "codex_session_jsonl",
            "parent_ctx_session_id": null,
            "root_ctx_session_id": SESSION_ID,
            "agent_scope": "primary",
            "suggested_next_commands": [
                format!("ctx show event {EVENT_ID} --window 10"),
                format!("ctx show session {SESSION_ID}"),
                format!("ctx search 'Unicode cache key' --session {SESSION_ID}"),
            ],
        }],
        "filters": {
            "primary_only": null,
        },
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

fn search_agent_value(
    agent_scope: Option<&str>,
    parent: Option<&str>,
    root: Option<&str>,
) -> Value {
    let mut value = compact_search_value();
    let result = value["results"][0]
        .as_object_mut()
        .expect("search fixture result is an object");
    if let Some(agent_scope) = agent_scope {
        result.insert("agent_scope".to_owned(), json!(agent_scope));
    } else {
        result.remove("agent_scope");
    }
    result.insert(
        "parent_ctx_session_id".to_owned(),
        parent.map_or(Value::Null, |parent| json!(parent)),
    );
    result.insert(
        "root_ctx_session_id".to_owned(),
        root.map_or(Value::Null, |root| json!(root)),
    );
    value
}

fn compact_search_value() -> Value {
    let mut value = search_value();
    let result = &mut value["results"][0];
    result["ctx_event_id"] = json!(EVENT_REF);
    result["ctx_session_id"] = json!(SESSION_REF);
    result["root_ctx_session_id"] = json!(SESSION_REF);
    result["suggested_next_commands"] = json!([
        format!("ctx show event {EVENT_REF} --window 10"),
        format!("ctx show session {SESSION_REF}"),
        format!("ctx search 'Unicode cache key' --session {SESSION_REF}"),
    ]);
    value
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
                "sequence": 17,
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
                "sequence": 18,
                "occurred_at": "2026-07-30T12:01:00.000Z",
                "text": "Done.",
            },
        ],
    })
}

fn locate_event_value() -> Value {
    json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_location",
        "ctx_event_id": EVENT_ID,
        "ctx_session_id": SESSION_ID,
        "provider": "codex",
        "provider_session_id": "demo-unicode-session",
        "provider_event_id": "native-event-17",
        "sequence": 17,
        "event_type": "message",
        "role": "user",
        "occurred_at": "2026-07-30T12:00:00.123Z",
        "source": {
            "ctx_source_id": "01900003-0000-7000-8000-000000000004",
            "source_format": "codex_session_jsonl",
            "schema_variant": "codex-rollout-v1",
            "provider_identity_version": 1,
        },
    })
}

fn locate_session_value() -> Value {
    json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_location",
        "ctx_session_id": SESSION_ID,
        "provider": "codex",
        "provider_session_id": "demo-unicode-session",
        "parent_ctx_session_id": null,
        "root_ctx_session_id": SESSION_ID,
        "started_at": "2026-07-30T12:00:00.123Z",
        "source": {
            "ctx_source_id": "01900003-0000-7000-8000-000000000004",
            "source_format": "codex_session_jsonl",
            "schema_variant": "codex-rollout-v1",
            "provider_identity_version": 1,
        },
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
    let mut plain = String::with_capacity(rendered.len());
    let mut characters = rendered.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' && matches!(characters.next(), Some('[')) {
            for control in characters.by_ref() {
                if ('@'..='~').contains(&control) {
                    break;
                }
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

#[test]
fn primary_renderers_match_reference_goldens_at_80_columns() {
    let context = context(80, ColorMode::Never);
    assert_eq!(
        render_search_document(&compact_search_value(), false, &context).render_plain(),
        include_str!("goldens/search.txt")
    );
    assert_eq!(
        render_show_document(&show_value(), &context).render_plain(),
        include_str!("goldens/show.txt")
    );
    assert_eq!(
        render_locate_document(&locate_event_value(), &context).render_plain(),
        include_str!("goldens/locate_event.txt")
    );
    assert_eq!(
        render_locate_document(&locate_session_value(), &context).render_plain(),
        include_str!("goldens/locate_session.txt")
    );
}

#[test]
fn search_heading_discloses_relevance_order_and_truthful_agent_scope() {
    for unicode in [true, false] {
        let separator = if unicode { " · " } else { " | " };
        let all_agents =
            render_search_document(&search_value(), false, &context_with_unicode(80, unicode))
                .render_plain();
        assert!(
            all_agents.starts_with(&format!(
                "1 result{separator}relevance order{separator}all agent sessions\n"
            )),
            "{all_agents}"
        );

        let mut primary_only = search_value();
        primary_only["filters"]["primary_only"] = json!(true);
        let primary_only =
            render_search_document(&primary_only, false, &context_with_unicode(80, unicode))
                .render_plain();
        assert!(primary_only.starts_with(&format!(
            "1 result{separator}relevance order{separator}primary sessions\n"
        )));
        assert!(!primary_only.contains("all agent sessions"));
    }

    let narrow = render_search_document(&search_value(), false, &context_with_unicode(32, true))
        .render_plain();
    assert!(narrow.starts_with("1 result\n  relevance order\n  all agent sessions\n"));

    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let first = render_search_document(&search_value(), false, &context);
        let second = render_search_document(&search_value(), false, &context);
        assert_eq!(first.render_plain(), second.render_plain());
        assert!(first.render_plain().contains("all agent sessions"));
        assert_fits(&first, &context);
    }
}

#[test]
fn normal_search_card_separates_provider_session_and_literal_root() {
    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_search_document(&compact_search_value(), false, &context);
        let rendered = document.render_plain();

        assert!(!rendered.contains("Agent"), "{rendered}");
        assert!(!rendered.contains("demo-unicode-session"), "{rendered}");
        assert_fits(&document, &context);
    }

    let rendered = render_search_document(
        &compact_search_value(),
        false,
        &context(120, ColorMode::Never),
    )
    .render_plain();
    assert!(rendered.contains("   Provider  Codex"), "{rendered}");
    assert!(
        rendered.contains(&format!("   Session   {SESSION_REF}")),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!("   Root      {SESSION_REF}")),
        "{rendered}"
    );
    assert!(!rendered.contains("   Source"), "{rendered}");
}

#[test]
fn normal_search_card_keeps_equal_parent_and_root_as_separate_claims() {
    let value = search_agent_value(
        Some("subagent"),
        Some(ROOT_SESSION_REF),
        Some(ROOT_SESSION_REF),
    );
    let document = render_search_document(&value, false, &context(120, ColorMode::Never));
    let rendered = document.render_plain();
    assert!(
        rendered.contains(&format!("   Parent    {ROOT_SESSION_REF}")),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!("   Root      {ROOT_SESSION_REF}")),
        "{rendered}"
    );
    assert_eq!(rendered.matches(ROOT_SESSION_REF).count(), 2, "{rendered}");
    assert!(!rendered.contains("Parent / root"), "{rendered}");
    assert!(!rendered.contains("Agent"), "{rendered}");
}

#[test]
fn normal_search_card_renders_nested_direct_lineage() {
    let value = search_agent_value(
        Some("subagent"),
        Some(PARENT_SESSION_REF),
        Some(ROOT_SESSION_REF),
    );

    for width in [32, 48, 80, 120] {
        let styled = context(width, ColorMode::Always);
        let plain = context(width, ColorMode::Never);
        let styled_document = render_search_document(&value, false, &styled);
        let plain_document = render_search_document(&value, false, &plain);
        assert_fits(&styled_document, &styled);
        assert_control_safe(&styled_document.render_plain());
        assert_eq!(
            strip_ansi(&styled_document.render(&styled)),
            plain_document.render_plain()
        );
        let references = styled_document
            .lines()
            .iter()
            .flat_map(|line| line.spans())
            .filter(|span| span.token() == Token::Reference)
            .map(|span| span.content())
            .collect::<Vec<_>>();
        assert!(references.contains(&PARENT_SESSION_REF), "{references:?}");
        assert!(references.contains(&ROOT_SESSION_REF), "{references:?}");
    }
}

#[test]
fn normal_search_card_renders_only_present_direct_lineage() {
    let cases = [
        (
            Some(PARENT_SESSION_REF),
            None,
            "   Parent    019000100",
            "   Root",
        ),
        (
            None,
            Some(ROOT_SESSION_REF),
            "   Root      019000101",
            "   Parent",
        ),
        (None, None, "   Session", "   Parent"),
    ];
    for (parent, root, expected, absent) in cases {
        let rendered = render_search_document(
            &search_agent_value(Some("subagent"), parent, root),
            false,
            &context(120, ColorMode::Never),
        )
        .render_plain();
        assert!(rendered.contains(expected), "{rendered}");
        assert!(!rendered.contains(absent), "{rendered}");
        if root.is_none() {
            assert!(!rendered.contains("   Root"), "{rendered}");
        }
        assert!(!rendered.contains("Agent"), "{rendered}");
    }
}

#[test]
fn normal_search_card_preserves_an_unresolved_full_reference() {
    let mut value = search_agent_value(Some("subagent"), Some(PARENT_SESSION_ID), None);
    value["results"][0]["ctx_event_id"] = json!(EVENT_ID);
    let document = render_search_document(&value, false, &context(120, ColorMode::Never));
    let rendered = document.render_plain();

    assert!(rendered.contains(PARENT_SESSION_ID), "{rendered}");
    assert!(rendered.contains(EVENT_ID), "{rendered}");
    assert!(document
        .lines()
        .iter()
        .flat_map(|line| line.spans())
        .any(|span| span.content() == PARENT_SESSION_ID && span.token() == Token::Reference));
}

#[test]
fn normal_search_card_renders_custom_source_identity_without_provider_duplication() {
    let mut custom = compact_search_value();
    custom["results"][0]["provider"] = json!("custom");
    custom["results"][0]["provider_key"] = json!("acme-history");
    custom["results"][0]["source_id"] = json!("workstation");
    let rendered =
        render_search_document(&custom, false, &context(120, ColorMode::Never)).render_plain();
    assert!(rendered.contains("   Provider  Custom"), "{rendered}");
    assert!(
        rendered.contains("   Source    acme-history/workstation"),
        "{rendered}"
    );

    custom["results"][0]["provider"] = json!("acme_history");
    custom["results"][0]["provider_key"] = json!("acme_history");
    custom["results"][0]["source_id"] = Value::Null;
    let rendered =
        render_search_document(&custom, false, &context(120, ColorMode::Never)).render_plain();
    assert!(rendered.contains("   Provider  acme history"), "{rendered}");
    assert!(!rendered.contains("   Source"), "{rendered}");
}

#[test]
fn search_card_dynamic_identity_fields_are_control_safe() {
    let mut value = search_agent_value(None, Some("parent\u{1b}[31m"), None);
    value["results"][0]["provider"] = json!("custom");
    value["results"][0]["provider_key"] = json!("plugin\u{1b}[2J");
    value["results"][0]["source_id"] = json!("line\nbreak\tvalue");
    let document = render_search_document(&value, false, &context(48, ColorMode::Never));
    let rendered = document.render_plain();
    assert!(
        rendered.contains("plugin\\x1b[2J/line\\nbreak\\tvalue"),
        "{rendered}"
    );
    assert!(rendered.contains("parent\\x1b[31m"), "{rendered}");
    assert_control_safe(&rendered);
    assert_fits(&document, &context(48, ColorMode::Never));
}

#[test]
fn normal_search_uses_compact_refs_and_verbose_uses_full_ctx_ids() {
    let mut ordinary_value = compact_search_value();
    ordinary_value["results"][0]["parent_ctx_session_id"] = json!(PARENT_SESSION_REF);
    ordinary_value["results"][0]["root_ctx_session_id"] = json!(ROOT_SESSION_REF);
    let ordinary = render_search_document(&ordinary_value, false, &context(120, ColorMode::Never))
        .render_plain();
    for expected in [SESSION_REF, EVENT_REF, PARENT_SESSION_REF, ROOT_SESSION_REF] {
        assert!(ordinary.contains(expected), "{ordinary}");
    }
    for unexpected in [SESSION_ID, EVENT_ID, PARENT_SESSION_ID, ROOT_SESSION_ID] {
        assert!(!ordinary.contains(unexpected), "{ordinary}");
    }

    let mut verbose_value = search_value();
    verbose_value["results"][0]["parent_ctx_session_id"] = json!(PARENT_SESSION_ID);
    verbose_value["results"][0]["root_ctx_session_id"] = json!(ROOT_SESSION_ID);
    let verbose = render_search_document(&verbose_value, true, &context(120, ColorMode::Never))
        .render_plain();
    for expected in [SESSION_ID, EVENT_ID, PARENT_SESSION_ID, ROOT_SESSION_ID] {
        assert!(verbose.contains(expected), "{verbose}");
    }
}

#[test]
fn search_event_row_uses_exact_milliseconds_and_quiet_missing_time() {
    let document = render_search_document(
        &compact_search_value(),
        false,
        &context(80, ColorMode::Never),
    );
    let rendered = document.render_plain();
    assert!(rendered.contains("Event     01900001 · 2026-07-30T12:00:00.123Z"));
    assert!(!rendered.contains("Match"));
    assert!(document
        .lines()
        .iter()
        .flat_map(|line| line.spans())
        .any(|span| span.content() == "01900001" && span.token() == Token::Reference));
    assert!(document
        .lines()
        .iter()
        .flat_map(|line| line.spans())
        .any(|span| {
            span.content() == "2026-07-30T12:00:00.123Z" && span.token() == Token::Text
        }));

    let ascii = render_search_document(
        &compact_search_value(),
        false,
        &context_with_unicode(80, false),
    )
    .render_plain();
    assert!(
        ascii.contains("Event     01900001 | 2026-07-30T12:00:00.123Z"),
        "{ascii}"
    );

    let mut missing = compact_search_value();
    missing["results"][0]["timestamp"] = Value::Null;
    let document = render_search_document(&missing, false, &context(32, ColorMode::Never));
    let rendered = document.render_plain();
    assert!(rendered.contains("Event     01900001"), "{rendered}");
    assert!(
        rendered.contains("Time      time unavailable"),
        "{rendered}"
    );
    assert!(document
        .lines()
        .iter()
        .flat_map(|line| line.spans())
        .any(|span| span.content() == "time unavailable" && span.token() == Token::Label));
}

#[test]
fn verbose_search_exposes_context_without_redundant_values_or_citation() {
    let ordinary = render_search_document(
        &compact_search_value(),
        false,
        &context(120, ColorMode::Never),
    )
    .render_plain();
    assert!(!ordinary.contains("Sequence"), "{ordinary}");
    assert!(ordinary.contains("Provider  Codex"), "{ordinary}");
    assert!(!ordinary.contains("Provider session"), "{ordinary}");
    assert!(!ordinary.contains("Agent"), "{ordinary}");

    let verbose = render_search_document(&search_value(), true, &context(120, ColorMode::Never))
        .render_plain();
    assert!(verbose.contains("Sequence          17"), "{verbose}");
    assert!(
        verbose.contains("Provider session  demo-unicode-session"),
        "{verbose}"
    );
    assert!(
        verbose.contains("Source format     codex_session_jsonl"),
        "{verbose}"
    );
    assert!(!verbose.contains("Citation"), "{verbose}");
    assert!(!verbose.contains("Agent"), "{verbose}");

    let mut nested = search_value();
    nested["results"][0]["agent_scope"] = json!("subagent");
    nested["results"][0]["parent_ctx_session_id"] = json!(PARENT_SESSION_ID);
    nested["results"][0]["root_ctx_session_id"] = json!(ROOT_SESSION_ID);
    let nested =
        render_search_document(&nested, true, &context(120, ColorMode::Never)).render_plain();
    for expected in [
        format!("Parent    {PARENT_SESSION_ID}"),
        format!("Root      {ROOT_SESSION_ID}"),
    ] {
        assert!(nested.contains(&expected), "{nested}");
    }
    assert_eq!(nested.matches(PARENT_SESSION_ID).count(), 1, "{nested}");
    assert_eq!(nested.matches(ROOT_SESSION_ID).count(), 1, "{nested}");
}

#[test]
fn show_and_locate_render_exact_chronology_and_missing_time() {
    let shown = render_show_document(&show_value(), &context(80, ColorMode::Never)).render_plain();
    assert!(shown.contains("Event  01900001-0000-7000-8000-000000000002 · seq 17"));
    let shown_ascii =
        render_show_document(&show_value(), &context_with_unicode(80, false)).render_plain();
    assert!(shown_ascii.contains("Event  01900001-0000-7000-8000-000000000002 | seq 17"));

    let mut missing_show = show_value();
    missing_show["events"][0]["occurred_at"] = Value::Null;
    let missing_show =
        render_show_document(&missing_show, &context(80, ColorMode::Never)).render_plain();
    assert!(missing_show.contains("Time   time unavailable"));

    let located = render_locate_document(&locate_event_value(), &context(80, ColorMode::Never))
        .render_plain();
    assert!(located.contains(&format!("Session           {SESSION_ID}")));
    assert!(located.contains("Time              2026-07-30T12:00:00.123Z"));
    assert!(located.contains("Sequence          17"));
    assert!(!located.contains("Role"));
    assert!(!located.contains("Type"));

    let session = render_locate_document(&locate_session_value(), &context(80, ColorMode::Never))
        .render_plain();
    assert!(session.contains("First event       2026-07-30T12:00:00.123Z"));
    assert!(!session.contains("Started"));

    let mut missing_locate = locate_event_value();
    missing_locate["occurred_at"] = Value::Null;
    let document = render_locate_document(&missing_locate, &context(80, ColorMode::Never));
    assert!(document
        .render_plain()
        .contains("Time              time unavailable"));
    assert!(document
        .lines()
        .iter()
        .flat_map(|line| line.spans())
        .any(|span| span.content() == "time unavailable" && span.token() == Token::Label));

    let mut missing_session = locate_session_value();
    missing_session["started_at"] = Value::Null;
    let missing_session =
        render_locate_document(&missing_session, &context(80, ColorMode::Never)).render_plain();
    assert!(missing_session.contains("First event       time unavailable"));
    assert!(!missing_session.contains("Started"));
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
    first["snippet"] = json!("First shaped result despite an older timestamp.");
    first["timestamp"] = json!("2025-01-01T00:00:00.000Z");
    let mut second = first.clone();
    second["rank"] = json!(2);
    second["retrieval_score"] = json!(9.5);
    second["session_importance"] = json!(9.5);
    second["ctx_event_id"] = json!(SECOND_EVENT_ID);
    second["snippet"] = json!("Second shaped result despite a newer timestamp.");
    second["timestamp"] = json!("2026-01-01T00:00:00.000Z");
    value["results"] = json!([first, second]);
    value["result_window"]["returned"] = json!(2);

    let rendered =
        render_search_document(&value, false, &context(80, ColorMode::Never)).render_plain();
    let first_position = rendered.find("1. First shaped result").unwrap();
    let second_position = rendered.find("2. Second shaped result").unwrap();
    assert!(first_position < second_position, "{rendered}");
    assert!(rendered.contains("2025-01-01T00:00:00.000Z"));
    assert!(rendered.contains("2026-01-01T00:00:00.000Z"));
    assert!(!rendered.contains("0.25") && !rendered.contains("9.50"));
}

#[test]
fn show_preserves_provider_sequence_when_timestamps_are_non_monotonic() {
    let mut value = show_value();
    value["events"][0]["occurred_at"] = json!("2026-07-30T12:02:00.000Z");
    value["events"][1]["occurred_at"] = json!("2026-07-30T12:01:00.000Z");
    let rendered = render_show_document(&value, &context(80, ColorMode::Never)).render_plain();

    assert!(
        rendered.find("1. user message").unwrap() < rendered.find("2. assistant message").unwrap()
    );
    assert!(rendered.find("seq 17").unwrap() < rendered.find("seq 18").unwrap());
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
fn empty_search_action_displays_a_positional_query() {
    let rendered =
        render_search_document(&empty_search_value(), false, &context(80, ColorMode::Never))
            .render_plain();
    assert!(rendered.contains("ctx search \"<term>\""));
    assert!(!rendered.contains("--term"));
}

#[test]
fn empty_exhausted_search_keeps_the_bounded_work_warning() {
    let mut value = empty_search_value();
    value["truncation"]["candidate_pool_truncated"] = json!(true);

    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_search_document(&value, false, &context);
        let rendered = document.render_plain();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(rendered.starts_with("No results for"));
        assert!(normalized.contains("Search reached a bounded candidate or work limit."));
        assert!(normalized.contains("Refine the query"));
        assert_fits(&document, &context);
    }
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
            render_locate_document(&locate_event_value(), &context),
            render_locate_document(&locate_session_value(), &context),
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
        assert!(normalized.contains("Search reached a bounded candidate or work limit."));
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
            render_locate_document(&locate_event_value(), &styled),
            render_locate_document(&locate_session_value(), &styled),
        ];
        let plain_documents = [
            render_search_document(&search_value(), false, &plain),
            render_search_document(&search_value(), true, &plain),
            render_search_document(&empty_search_value(), false, &plain),
            render_show_document(&show_value(), &plain),
            render_locate_document(&locate_event_value(), &plain),
            render_locate_document(&locate_session_value(), &plain),
        ];

        for (styled_document, plain_document) in styled_documents.into_iter().zip(plain_documents) {
            let styled_output = styled_document.render(&styled);
            assert!(styled_output.contains("\u{1b}["));
            assert_eq!(strip_ansi(&styled_output), plain_document.render_plain());
        }
    }
}

#[test]
fn human_rendering_does_not_change_machine_values_or_compatibility_bytes() {
    let search = search_value();
    let show = show_value();
    let locate = locate_event_value();
    let before = [
        serde_json::to_vec(&search).unwrap(),
        serde_json::to_vec(&show).unwrap(),
        serde_json::to_vec(&locate).unwrap(),
    ];

    let _ = render_search_document(&search, true, &context(80, ColorMode::Always));
    let _ = render_show_document(&show, &context(80, ColorMode::Always));
    let _ = render_locate_document(&locate, &context(80, ColorMode::Always));

    assert_eq!(before[0], serde_json::to_vec(&search).unwrap());
    assert_eq!(before[1], serde_json::to_vec(&show).unwrap());
    assert_eq!(before[2], serde_json::to_vec(&locate).unwrap());
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
fn activity_is_machine_exact_and_human_control_safe() {
    let activity = json!({
        "revision": 1,
        "provider_call_id": {"utf8": "literal\\n\n# heading\u{202e}\u{2066}\u{1b}[2J|` then `` then ``` preserved"},
        "invocation": {
            "protocol": "mcp",
            "server": "source-server",
            "tool": "tool\\literal\t*#_{}<>",
            "arguments": {"capture_status": "present", "value": {"key": "雪"}}
        },
        "facts": [
            {"kind": "file", "value": "src/lib.rs"},
            {"kind": "file", "value": "src/lib.rs"}
        ]
    });
    let mut session = show_value();
    session["events"][0]["activity"] = activity.clone();
    let mut event = event_show_value(None, SESSION_ID);
    event["events"][0]["activity"] = activity.clone();

    for value in [&session, &event] {
        let jsonl = render_show_jsonl(value).unwrap();
        let first: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        let machine_event = first.get("event").unwrap_or(&first);
        assert_eq!(machine_event["activity"], activity);
        assert!(machine_event["activity"]["provider_call_id"]["utf8"]
            .as_str()
            .unwrap()
            .contains('\u{202e}'));
        let lines = jsonl.lines().collect::<Vec<_>>();
        let absent: Value = serde_json::from_str(lines[1]).unwrap();
        let absent_event = absent.get("event").unwrap_or(&absent);
        assert!(absent_event.get("activity").is_none());
    }

    let terminal = render_show_document(&session, &context(200, ColorMode::Never)).render_plain();
    let text = render_show_text(&session);
    let safe_activity = safe_activity_json(&activity);
    assert!(safe_activity.contains("\\u{202e}"), "{safe_activity:?}");
    assert!(safe_activity.contains("\\u{2066}"), "{safe_activity:?}");
    assert!(safe_activity.contains("` then `` then ``` preserved"));
    assert!(!safe_activity.contains('\u{202e}'));
    assert!(!safe_activity.contains('\u{2066}'));
    for rendered in [&terminal, &text] {
        assert_control_safe(rendered);
        assert!(!rendered.contains('\u{202e}'));
        assert!(!rendered.contains('\u{2066}'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("\\u{202e}"), "{rendered:?}");
        assert!(rendered.contains("\\u{2066}"), "{rendered:?}");
        assert!(rendered.contains("src/lib.rs"), "{rendered:?}");
    }
    assert!(terminal.contains("Activity"), "{terminal:?}");
    assert!(text.contains("activity:"), "{text:?}");

    let markdown = render_show_markdown(&session);
    assert_control_safe(&markdown);
    assert!(!markdown.contains('\u{202e}'));
    assert!(!markdown.contains('\u{2066}'));
    assert!(!markdown.contains('\u{1b}'));
    assert!(!markdown.contains("\n# heading"));
    assert!(markdown.contains(&format!("activity: ````{safe_activity}````\n\n")));
    assert!(markdown.contains("src/lib.rs"));
}

#[test]
fn markdown_activity_code_span_uses_a_longer_backtick_delimiter_without_rewriting_content() {
    let content = r#"{"single":"`","double":"``","triple":"```"}"#;

    assert_eq!(markdown_code_span(content), format!("````{content}````"));
    assert_eq!(
        markdown_code_span(r#"{"plain":true}"#),
        r#"`{"plain":true}`"#
    );
}

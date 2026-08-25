use super::*;
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CoreActivity,
    CoreContent, CoreContentPolicyStatus, LiteralFactKind, ProviderDeclaredFact,
    CORE_ACTIVITY_REVISION, CORE_CONTENT_POLICY_REVISION,
};

fn selected_projection(
    body: Option<String>,
    structured_content: Option<serde_json::Value>,
    activity: Option<CoreActivity>,
) -> SearchContentProjection {
    project_search_content(CoreContent {
        policy_revision: CORE_CONTENT_POLICY_REVISION,
        policy_status: CoreContentPolicyStatus::Selected,
        normalized_body: body,
        structured_content,
        discovery_exclusion: None,
        activity,
    })
    .unwrap()
    .unwrap()
}
fn invocation_and_result(invocation: serde_json::Value, result: &str) -> CoreActivity {
    CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: Some(ActivityInvocation {
            protocol: Some("mcp".to_owned()),
            server: Some("demo".to_owned()),
            tool: "search".to_owned(),
            arguments: ActivityJsonCapture::Present { value: invocation },
            started_at_unix_ms: None,
        }),
        result: Some(ActivityResult {
            status: Some("ok".to_owned()),
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Present {
                value: result.to_owned(),
            },
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: Vec::new(),
    }
}
#[test]
fn fragment_excerpt_favors_the_tight_window_with_most_analyzed_terms() {
    let body = format!(
        "alpha appears early. {}The decisive clause contains alpha beta gamma together. {}",
        "filler word ".repeat(90),
        "closing material ".repeat(40)
    );
    let projection = selected_projection(Some(body), None, None);
    let (snippet, truncated) = fragment_aware_search_excerpt(&projection, &["alpha beta gamma"]);
    assert!(truncated);
    assert!(snippet.starts_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(snippet.ends_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(snippet.contains("decisive clause"));
    assert!(snippet.contains("alpha beta gamma"));
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
    let visible = snippet.trim_matches('…');
    assert!(visible.starts_with("filler") || visible.starts_with("The decisive"));
}
#[test]
fn fragment_excerpt_prefers_readable_result_over_invocation_echo() {
    let projection = selected_projection(
        None,
        None,
        Some(invocation_and_result(
            serde_json::json!({"query": "needle invocation echo"}),
            "The readable result explains the needle decisively.",
        )),
    );
    let (snippet, truncated) = fragment_aware_search_excerpt(&projection, &["needle"]);
    assert!(truncated);
    assert!(snippet.contains("readable result"));
    assert!(!snippet.contains("invocation echo"));
    assert!(snippet.starts_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(!snippet.ends_with(SEARCH_EXCERPT_ELLIPSIS));
}
#[test]
fn fragment_excerpt_retains_an_invocation_only_match() {
    let projection = selected_projection(
        None,
        None,
        Some(invocation_and_result(
            serde_json::json!({"query": "needle echo only"}),
            "unrelated terminal text",
        )),
    );
    let (snippet, truncated) = fragment_aware_search_excerpt(&projection, &["needle"]);
    assert!(truncated);
    assert!(snippet.contains("needle echo only"));
}
#[test]
fn fragment_excerpt_decodes_json_string_scalar_without_wrapper_escapes() {
    let display = "escaped \"wrapper\"\nwith a decisive clause";
    let projection = selected_projection(None, Some(serde_json::json!(display)), None);
    let ((snippet, truncated), work) =
        fragment_aware_search_excerpt_with_work(&projection, &["decisive clause"]);
    assert_eq!(snippet, display);
    assert!(!truncated);
    assert!(!snippet.contains("\\\""));
    assert!(!snippet.contains("\\n"));
    assert!(work.decoded_analyzed_tokens > 0);
    assert!(work.decoded_fit_bytes_traversed <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(work.decoded_fit_graphemes_traversed <= SEARCH_SNIPPET_MAX_CHARS);
}
#[test]
fn fragment_excerpt_falls_back_to_serialized_json_for_escape_only_match() {
    let projection = selected_projection(None, Some(serde_json::json!("\nclause")), None);
    let (snippet, truncated) = fragment_aware_search_excerpt(&projection, &["nclause"]);
    assert_eq!(snippet, "\"\\nclause\"");
    assert!(!truncated);
}
#[test]
fn fragment_excerpt_preserves_unicode_graphemes_and_cjk_analyzer_terms() {
    let combining = "e\u{301}";
    let body = format!(
        "{} 数据库迁移 decisive result {}",
        format!("{combining} ").repeat(240),
        format!(" {combining}").repeat(240)
    );
    let projection = selected_projection(Some(body), None, None);
    let (snippet, truncated) = fragment_aware_search_excerpt(&projection, &["数据库迁移"]);
    assert!(truncated);
    assert!(snippet.contains("数据库迁移"));
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    let visible_graphemes = snippet
        .trim_matches('…')
        .graphemes(true)
        .collect::<Vec<_>>();
    assert_ne!(visible_graphemes.first().copied(), Some("\u{301}"));
    assert_ne!(visible_graphemes.last().copied(), Some("\u{301}"));
}
#[test]
fn fragment_excerpt_trims_only_at_full_text_indic_grapheme_boundaries() {
    const CONJUNCT: &str = "\u{915}\u{94d}\u{937}";
    const INNER_CONSONANT: &str = "\u{937}";
    let body = format!("{}{CONJUNCT} {}", "a".repeat(10_000), "b".repeat(10_000));
    assert!(body.len() > SEARCH_SNIPPET_MAX_BYTES);
    let projection = selected_projection(Some(body.clone()), None, None);

    let ((snippet, truncated), work) =
        fragment_aware_search_excerpt_with_work(&projection, &[INNER_CONSONANT]);

    assert!(truncated);
    assert!(snippet.starts_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(snippet.ends_with(SEARCH_EXCERPT_ELLIPSIS));
    let visible = snippet
        .strip_prefix(SEARCH_EXCERPT_ELLIPSIS)
        .and_then(|value| value.strip_suffix(SEARCH_EXCERPT_ELLIPSIS))
        .expect("the forced middle excerpt has both ellipses");
    assert!(visible.contains(CONJUNCT));
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(work.local_context_bytes_traversed <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(work.local_context_graphemes_traversed <= SEARCH_SNIPPET_MAX_CHARS);

    let internal_token_start = body.find(INNER_CONSONANT).unwrap();
    let is_full_text_boundary = |offset| {
        offset == body.len()
            || body
                .grapheme_indices(true)
                .any(|(boundary, _)| boundary == offset)
    };
    assert!(!is_full_text_boundary(internal_token_start));
    let start = body.find(visible).unwrap();
    let end = start.saturating_add(visible.len());
    assert!(start > 0 && end < body.len());
    assert!(is_full_text_boundary(start));
    assert!(is_full_text_boundary(end));
}
#[test]
fn fragment_excerpt_bounds_long_identifiers_and_pathological_clusters() {
    let identifier = "TechnicalIdentifier".repeat(28);
    let projection = selected_projection(Some(identifier.clone()), None, None);
    let (identifier_excerpt, identifier_truncated) =
        fragment_aware_search_excerpt(&projection, &[&identifier]);
    assert!(identifier_truncated);
    assert!(identifier_excerpt.starts_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(!identifier_excerpt.ends_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(identifier.ends_with(identifier_excerpt.trim_start_matches(SEARCH_EXCERPT_ELLIPSIS)));
    assert!(identifier_excerpt.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(identifier_excerpt.len() <= SEARCH_SNIPPET_MAX_BYTES);
    let oversized_cluster = format!("x{}", "\u{301}".repeat(SEARCH_SNIPPET_MAX_BYTES));
    let body = format!("{oversized_cluster} needle result");
    let projection = selected_projection(Some(body), None, None);
    let (cluster_excerpt, cluster_truncated) =
        fragment_aware_search_excerpt(&projection, &["needle"]);
    assert!(cluster_truncated);
    assert!(cluster_excerpt.contains("needle"));
    assert!(cluster_excerpt.starts_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(cluster_excerpt.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(cluster_excerpt.len() <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(!cluster_excerpt.starts_with('\u{301}'));
}
#[test]
fn snippet_centers_the_actual_case_insensitive_match() {
    let body = format!("{}NeEdLe {}", "alpha ".repeat(750), "omega ".repeat(750));
    let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);
    assert!(truncated);
    assert!(snippet.contains("NeEdLe"));
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(!snippet.starts_with("lpha"));
}
#[test]
fn snippet_preserves_combining_and_emoji_graphemes() {
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
fn snippet_byte_bounds_pathological_grapheme_clusters() {
    let oversized_cluster = format!("x{}", "\u{301}".repeat(SEARCH_SNIPPET_MAX_BYTES));
    // Keep the queried term analyzer-distinct from the `x` that anchors each
    // oversized combining cluster.
    let body = format!("{oversized_cluster} needle {oversized_cluster}");
    let (snippet, truncated) = search_snippet_fragment(&body, &["needle"]);
    assert!(truncated);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(snippet.contains("needle"), "snippet was {snippet:?}");
    assert_eq!(
        search_snippet_fragment(&oversized_cluster, &["x"]),
        (String::new(), true)
    );
}

#[test]
fn fragment_excerpt_skips_an_unrenderable_match_for_a_bounded_alternative() {
    let oversized_cluster = format!("x{}", "\u{301}".repeat(SEARCH_SNIPPET_MAX_BYTES));
    let activity = CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::Workspace,
            value: "x metadata fallback".to_owned(),
        }],
    };
    let projection = selected_projection(Some(oversized_cluster), None, Some(activity));

    let (snippet, truncated) = fragment_aware_search_excerpt(&projection, &["x"]);
    assert_eq!(snippet, "…x metadata fallback");
    assert!(truncated);
}

#[test]
fn snippet_handles_a_maximum_valid_core_body_without_offset_vectors() {
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
fn dense_repeated_matches_use_one_term_lookup_per_token_and_bounded_local_state() {
    let query = (0..SEARCH_EXCERPT_MAX_QUERY_TERMS)
        .map(|term| format!("term{term:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    let body = format!("{query} ").repeat(2_000);
    let projection = selected_projection(Some(body), None, None);

    let ((snippet, truncated), work) =
        fragment_aware_search_excerpt_with_work(&projection, &[&query]);

    assert!(truncated);
    assert!(snippet.contains("term00"));
    assert!(snippet.contains("term31"));
    assert!(work.analyzed_tokens > 50_000);
    assert_eq!(work.query_membership_lookups, work.analyzed_tokens);
    assert!(work.peak_retained_occurrences <= SEARCH_EXCERPT_MAX_LOCAL_OCCURRENCES);
    assert!(work.peak_retained_graphemes <= SEARCH_SNIPPET_MAX_CHARS);
    assert_eq!(work.local_context_calls, 1);
    assert!(work.local_context_bytes_traversed <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(work.local_context_graphemes_traversed <= SEARCH_SNIPPET_MAX_CHARS);
}

#[test]
fn maximum_core_body_keeps_only_the_bounded_local_match_window() {
    let suffix = " needle";
    let mut body = "x".repeat(MAX_CORE_CONTENT_BYTES - suffix.len());
    body.push_str(suffix);
    let projection = selected_projection(Some(body), None, None);
    let projected_bytes = projection.index_text().len();

    let ((snippet, truncated), work) =
        fragment_aware_search_excerpt_with_work(&projection, &["needle"]);

    assert!(truncated);
    assert!(snippet.contains("needle"));
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(work.analyzed_tokens > 100_000);
    assert_eq!(work.query_membership_lookups, work.analyzed_tokens);
    assert!(
        work.alignment_bytes_traversed > projected_bytes.saturating_sub(SEARCH_SNIPPET_MAX_BYTES)
    );
    assert!(work.alignment_graphemes_traversed > 100_000);
    assert_eq!(work.local_context_calls, 1);
    assert!(work.local_context_bytes_traversed <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(work.local_context_graphemes_traversed <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(work.peak_retained_occurrences <= SEARCH_EXCERPT_MAX_LOCAL_OCCURRENCES);
    assert!(work.peak_retained_graphemes <= SEARCH_SNIPPET_MAX_CHARS);
}

#[test]
fn large_json_scalar_uses_only_exact_analysis_and_bounded_local_context() {
    let display = format!(
        "{} decisive needle",
        "large serialized scalar filler ".repeat(2_500)
    );
    assert!(display.len() > SEARCH_SNIPPET_MAX_BYTES);
    let projection = selected_projection(None, Some(serde_json::json!(display)), None);

    let ((snippet, truncated), work) =
        fragment_aware_search_excerpt_with_work(&projection, &["decisive needle"]);

    assert!(truncated);
    assert!(snippet.starts_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(!snippet.ends_with(SEARCH_EXCERPT_ELLIPSIS));
    assert!(snippet.contains("decisive needle"));
    assert!(snippet.ends_with('"'));
    assert_eq!(work.decoded_analyzed_tokens, 0);
    assert_eq!(work.decoded_fit_bytes_traversed, 0);
    assert_eq!(work.decoded_fit_graphemes_traversed, 0);
    assert_eq!(work.local_context_calls, 1);
    assert!(work.local_context_bytes_traversed <= SEARCH_SNIPPET_MAX_BYTES);
    assert!(work.local_context_graphemes_traversed <= SEARCH_SNIPPET_MAX_CHARS);
    assert_eq!(work.query_membership_lookups, work.analyzed_tokens);
}

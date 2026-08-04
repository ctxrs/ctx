"""Authoritative public MCP attribution executable capabilities."""

MCP_ATTRIBUTION_EVIDENCE_CLASSES = [
    "ambiguity_duplicate_linkage",
    "canonical_terminal_outcomes",
    "exact_boundary",
    "exact_positive_pair",
    "malformed_identity",
    "max_plus_one",
    "privacy_sinks",
    "result_preservation",
    "search_nonindexing",
    "stable_ids",
]

# Every public suite alias is bound to one physical Bazel target and the exact
# test functions/classes it may claim. Target identity is passed into the
# runner so aliases cannot manufacture additional evidence identities.
# Closed suites must claim the binary's complete `--list` inventory. Selected
# aliases bind named tests in a larger existing Rust target; the runner proves
# each name exists and executes every claimed test with libtest `--exact`.
MCP_ATTRIBUTION_PUBLIC_SUITES = {
    "codex_direct_result": struct(
        target = "//crates/ctx-history-capture:codex_direct_result_tests",
        selected_inventory = False,
        tests = {
            "appended_duplicate_terminal_retracts_prior_attribution_and_preserves_ids": ["ambiguity_duplicate_linkage"],
            "appended_malformed_duplicate_retracts_attribution_without_touching_neighbor_ids": ["ambiguity_duplicate_linkage"],
            "exact_error_attribution_and_ambiguous_pair_abstention_survive_publication": ["canonical_terminal_outcomes"],
            "exact_raw_limit_omits_oversized_invocation_but_publishes_result": ["max_plus_one"],
            "invalid_attribution_preserves_terminal_content_and_all_stable_identities": ["stable_ids"],
            "malformed_mcp_results_are_rejected_without_hiding_later_valid_content": ["malformed_identity"],
            "mcp_attribution_canaries_are_not_indexed_or_ranked": ["search_nonindexing"],
            "malformed_duplicate_terminals_abstain_without_losing_public_content_or_ids": ["malformed_identity"],
            "over_8_mib_mcp_result_is_admitted_once_and_indexable": ["result_preservation"],
            "redacted_real_shape_fixture_is_admitted_with_linkage_and_metadata": ["exact_positive_pair"],
        },
    ),
    "mcp_attribution_core": struct(
        target = "//crates/ctx-history-core:unit_tests",
        selected_inventory = True,
        tests = {
            "core_record::tests::mcp_tool_call_bounds_each_decoded_utf8_component_at_exact_64_kib": ["max_plus_one"],
        },
    ),
    "mcp_attribution_provider_units": struct(
        target = "//crates/ctx-history-capture:unit_tests",
        selected_inventory = True,
        tests = {
            "provider::codex::nativepath::tests::profiles::exact_mcp_attribution_preserves_opaque_names_and_component_bound": ["exact_boundary"],
            "provider::providers::native_jsonl::native_path::source_backed::copilot_tests::copilot_attribution_does_not_change_stable_event_ids": ["stable_ids"],
            "provider::providers::native_jsonl::native_path::source_backed::copilot_tests::copilot_attributes_only_unique_exact_terminal_completions": ["canonical_terminal_outcomes"],
            "provider::providers::native_jsonl::native_path::source_backed::copilot_tests::copilot_late_duplicate_retracts_the_previously_attributed_completion": ["ambiguity_duplicate_linkage"],
            "provider::providers::native_jsonl::native_path::source_backed::copilot_tests::copilot_malformed_ambiguous_or_orphan_linkage_abstains": ["malformed_identity"],
            "provider::providers::native_jsonl::native_path::source_backed::copilot_tests::copilot_oversized_or_malformed_linkage_never_aborts_neighbor_projection": ["result_preservation"],
            "provider::providers::native_jsonl::native_path::source_backed::copilot_tests::copilot_same_call_id_in_separate_sessions_remains_independent": ["exact_positive_pair"],
            "provider::providers::warp::nativepath::decode::tests::invalid_duplicate_orphan_and_ambiguous_mcp_relations_abstain": ["ambiguity_duplicate_linkage"],
            "provider::providers::warp::nativepath::decode::tests::invalid_then_valid_required_strings_permanently_invalidate_attribution": ["malformed_identity"],
            "provider::providers::warp::nativepath::decode::tests::qualified_mcp_success_error_cancellation_and_nontext_results_link_exactly": ["exact_positive_pair"],
            "provider::providers::warp::nativepath::decode::tests::textual_success_failure_and_unknown_results_are_complete": ["canonical_terminal_outcomes"],
            "provider::providers::warp::nativepath::decode::tests::validated_uuid_text_is_preserved_exactly": ["exact_boundary"],
            "provider::providers::warp::source_backed::result_tests::core_projection_keeps_success_failure_unknown_and_large_result_bodies_once": ["result_preservation"],
            "provider::providers::warp::source_backed::result_tests::sanitized_mcp_fixture_projects_only_unique_qualified_terminal_pairs": ["stable_ids"],
            "provider::source_backed::tests::copilot::copilot_route_enforces_independent_exact_identity_component_boundaries": ["exact_boundary"],
        },
    ),
    "mcp_attribution_privacy": struct(
        target = "//crates/ctx-cli:mcp_attribution_privacy_tests",
        selected_inventory = False,
        tests = {
            "mcp_attribution_canaries_stay_out_of_search_analytics_usage_and_diagnostics": ["privacy_sinks"],
        },
    ),
    "mcp_attribution_search": struct(
        target = "//crates/ctx-history-index:mcp_attribution_search_tests",
        selected_inventory = False,
        tests = {
            "mcp_tool_call_attribution_is_stored_but_never_indexed_or_ranked": ["search_nonindexing"],
        },
    ),
}

def _checked_public_suite(suite_id):
    suite = MCP_ATTRIBUTION_PUBLIC_SUITES[suite_id]
    if not suite.target.startswith("//") or ":" not in suite.target:
        fail("public MCP attribution suite %s must name an absolute Bazel target" % suite_id)
    if type(suite.selected_inventory) != "bool":
        fail("public MCP attribution suite %s must declare selected_inventory as a bool" % suite_id)
    if not suite.tests:
        fail("public MCP attribution suite %s has zero tests" % suite_id)
    for test_name in suite.tests:
        if not test_name:
            fail("public MCP attribution suite %s has an empty test name" % suite_id)
        classes = suite.tests[test_name]
        if len(classes) != 1:
            fail("public MCP attribution test %s::%s must claim exactly one capability" % (suite_id, test_name))
        for evidence_class in classes:
            if evidence_class not in MCP_ATTRIBUTION_EVIDENCE_CLASSES:
                fail("public MCP attribution test %s::%s has unknown capability %s" % (suite_id, test_name, evidence_class))
    return suite

def mcp_attribution_suite_args():
    args = ["--mode", "public-validation"]
    targets = {}
    for suite_id in sorted(MCP_ATTRIBUTION_PUBLIC_SUITES):
        suite = _checked_public_suite(suite_id)
        if suite.target in targets:
            fail("public MCP attribution suites %s and %s reuse physical target %s" % (targets[suite.target], suite_id, suite.target))
        targets[suite.target] = suite_id
        args.extend([
            "--suite-alias" if suite.selected_inventory else "--suite",
            "%s=%s=$(rootpath %s)" % (suite_id, suite.target, suite.target),
        ])
        for test_name in sorted(suite.tests):
            args.extend([
                "--test-capability",
                "%s::%s=%s" % (suite_id, test_name, ",".join(sorted(suite.tests[test_name]))),
            ])
    return args

def mcp_attribution_suite_data():
    return sorted([
        _checked_public_suite(suite_id).target
        for suite_id in MCP_ATTRIBUTION_PUBLIC_SUITES
    ])

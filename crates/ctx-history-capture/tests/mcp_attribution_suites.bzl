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
    "stable_ids",
]

# Every public suite alias is bound to one physical Bazel target and the exact
# test functions/classes it may claim. Target identity is passed into the
# runner so aliases cannot manufacture additional evidence identities.
# Closed suites must claim the binary's complete `--list` inventory. Selected
# aliases bind named tests in a larger existing Rust target; the runner proves
# each name exists and executes every claimed test with libtest `--exact`.
MCP_ATTRIBUTION_PUBLIC_SUITES = {
    "mcp_attribution_core": struct(
        target = "//crates/ctx-history-core:unit_tests",
        selected_inventory = True,
        tests = {
            "core_record::tests::activity_metadata_accepts_exact_boundaries_and_rejects_max_plus_one": ["max_plus_one"],
        },
    ),
    "mcp_attribution_capture_provider_units": struct(
        target = "//crates/ctx-history-capture-composition-qualification:jsonl_publication_tests",
        selected_inventory = True,
        tests = {
            "copilot::copilot_activity_append_replay_preserves_stable_event_ids": ["stable_ids"],
            "copilot::copilot_route_enforces_independent_exact_identity_component_boundaries": ["exact_boundary"],
        },
    ),
    "mcp_attribution_capture_provider_lifecycle": struct(
        target = "//crates/ctx-history-capture-composition-qualification:provider_lifecycle_tests",
        selected_inventory = True,
        tests = {
            "codex_child_independence::lifecycle::codex_mcp_activity_append_replay_preserves_stable_ids_and_exact_content": ["stable_ids"],
        },
    ),
    "mcp_attribution_codex_provider_units": struct(
        target = "//crates/ctx-history-provider-codex:unit_tests",
        selected_inventory = True,
        tests = {
            "codex::nativepath::rows::tests::duplicate_selectors_withhold_linkage_and_preserve_raw_fact_order": ["ambiguity_duplicate_linkage"],
            "codex::nativepath::rows::tests::empty_result_string_is_absent_text_with_exact_structured_capture": ["result_preservation"],
            "codex::nativepath::rows::tests::exact_mcp_identity_boundary_is_accepted_and_max_plus_one_abstains": ["exact_boundary"],
            "codex::nativepath::rows::tests::malformed_mcp_identity_abstains_without_losing_valid_result_activity": ["malformed_identity"],
            "codex::nativepath::rows::tests::mcp_terminal_activity_preserves_exact_server_tool_and_linkage": ["exact_positive_pair"],
            "codex::nativepath::rows::tests::terminal_outcomes_preserve_literal_status_and_complete_result_content": ["canonical_terminal_outcomes"],
        },
    ),
    "mcp_attribution_native_jsonl_provider_units": struct(
        target = "//crates/ctx-history-provider-native-jsonl:unit_tests",
        selected_inventory = True,
        tests = {
            "native_path::source_backed::copilot_tests::absent_and_ambiguous_capture_states_are_explicit": ["ambiguity_duplicate_linkage"],
            "native_path::source_backed::copilot_tests::completion_preserves_literal_result_without_inferred_status": ["result_preservation"],
            "native_path::source_backed::copilot_tests::invocation_preserves_exact_native_identity_and_arguments": ["exact_positive_pair"],
            "native_path::source_backed::copilot_tests::malformed_or_oversized_identity_abstains_without_poisoning_valid_input": ["malformed_identity"],
            "native_path::source_backed::copilot_tests::success_failure_and_unknown_outcomes_remain_literal_without_inferred_status": ["canonical_terminal_outcomes"],
        },
    ),
    "mcp_attribution_selected_sqlite_provider_units": struct(
        target = "//crates/ctx-history-providers-sqlite-selected:unit_tests",
        selected_inventory = True,
        tests = {
            "providers::warp::nativepath::decode::tests::invalid_duplicate_orphan_and_ambiguous_mcp_relations_abstain": ["ambiguity_duplicate_linkage"],
            "providers::warp::nativepath::decode::tests::invalid_then_valid_required_strings_permanently_invalidate_attribution": ["malformed_identity"],
            "providers::warp::nativepath::decode::tests::qualified_mcp_success_error_cancellation_and_nontext_results_link_exactly": ["exact_positive_pair"],
            "providers::warp::nativepath::decode::tests::textual_success_failure_and_unknown_results_are_complete": ["canonical_terminal_outcomes"],
            "providers::warp::nativepath::decode::tests::validated_uuid_text_is_preserved_exactly": ["exact_boundary"],
            "providers::warp::source_backed::result_tests::core_projection_keeps_success_failure_unknown_and_large_result_bodies_once": ["result_preservation"],
            "providers::warp::source_backed::result_tests::sanitized_mcp_fixture_projects_only_unique_qualified_terminal_pairs": ["stable_ids"],
        },
    ),
    "mcp_attribution_privacy": struct(
        target = "//crates/ctx-agent-application:mcp_attribution_privacy_tests",
        selected_inventory = False,
        tests = {
            "mcp_activity_is_searchable_but_stays_out_of_analytics_usage_and_diagnostics": ["privacy_sinks"],
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

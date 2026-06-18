load("@rules_rust//rust:defs.bzl", "rust_test")
load("@rules_shell//shell:sh_test.bzl", "sh_test")

CTX_HTTP_SUITE_ORDER = [
    "workspace-stream",
    "provider-auth",
    "provider-runtime-simulated",
    "provider-runtime-live",
    "repo-vcs",
    "scheduler-runtime",
    "turns-terminal",
    "attachments-routing",
    "subagents-control",
    "subagents-local-runtime",
    "updates-release",
    "sandbox-runtime-simulated",
    "sandbox-runtime-container-e2e",
    "sandbox-runtime-resource-governance",
    "sandbox-runtime-memory-leak",
]

CTX_HTTP_SUITE_TESTS = {
    "workspace-stream": [
        "cache_rehydration",
        "fault_matrix",
        "hot_endpoints_no_db",
        "replay_properties",
        "task_default_session_http",
        "workspace_active_snapshot_http",
        "workspace_active_snapshot_http_workspace_vcs_stream_repeat_subscribe_preserves_ready_worktree_vcs_state",
        "workspace_active_snapshot_http_workspace_vcs_stream_subscribe_does_not_reemit_when_worktree_vcs_is_already_computing",
        "workspace_active_snapshot_http_worktree_vcs_summary_refresh_reloads_live_inventory_before_ready_publish",
        "workspace_stream_context_window_metrics",
        "workspace_stream_no_gaps_under_activity",
        "workspace_stream_stress_active_heads_lag",
    ],
    "provider-auth": [
        "acp_target_scoped_status",
        "codex_host_import_api",
        "codex_login_callback_api",
        "install_start_contract",
        "provider_current_ctx_version_regressions",
        "provider_target_scoped_installs",
        "subscription_accounts_api",
    ],
    "provider-runtime-simulated": [
        "provider_probe_runtime_env",
        "provider_worker_reaping_offline",
        "provider_scenarios_offline_crp_fixtures",
        "provider_scenarios_offline_interleaved_assistant_tools_do_not_fragment_messages",
        "provider_scenarios_offline_crp_fixtures_persist_context_window_metrics",
        "session_model_api",
        "workspace_provider_model_preferences_http",
    ],
    "provider-runtime-live": [
        "acp_crp_bridge_tokens_e2e",
        "gemini_live_model_catalog",
        "live_provider_canary",
    ],
    "repo-vcs": [
        "jj_merge_queue_basics",
        "merge_queue_entry_routes_http",
        "merge_queue_isolation",
        "repo_clone_branch_and_safety",
        "repo_init_initial_commit",
        "repo_status_and_staging",
        "repo_validate_destination",
        "session_diff_unavailable",
        "workspace_execution_config_http",
        "workspace_file_completions_http",
        "workspace_merge_queue_config_http",
        "worktree_bootstrap_config_http",
        "worktree_archive_http",
        "worktree_vcs_snapshot",
    ],
    "scheduler-runtime": [
        "assistant_chunk_stream_only",
        "assistant_message_persistence_faults",
        "noisy_output_backpressure",
        "turn_lifecycle_events",
        "turn_terminal_reconciliation",
    ],
    "turns-terminal": [
        "demo_seed_transcript_http",
        "message_idempotency_post_message_idempotent_same_payload",
        "message_idempotency_post_message_idempotent_conflict_on_change",
        "terminal_rest_route_contracts",
        "terminal_workspace_stream_separation",
        "terminal_ws_reconnect",
    ],
    "attachments-routing": [
        "global_id_routing_http",
        "image_attachments_http_e2e",
        "workspace_attachments_local_canonical",
    ],
    "subagents-control": [
        "subagent_mcp_http",
        "subagent_mcp_http_archive_agent_reclaims_dedicated_child_worktree",
        "system_prompt_append_http",
        "title_generation_local",
    ],
    "subagents-local-runtime": [
        "title_generation_local_e2e",
    ],
    "updates-release": [
        "openai_responses_sse_stub",
        "updates_appimage_apply_safety",
        "updates_failure_safety_checksum_mismatch",
        "updates_failure_safety_interrupted_transfer",
        "release_manifest_corpus",
        "updates_failure_safety_manifest_parse",
        "updates_failure_safety_manifest_signature",
        "updates_failure_safety_missing_artifact",
    ],
    "sandbox-runtime-simulated": [
        "workspace_runtime_crash_recovery",
    ],
    "sandbox-runtime-container-e2e": [
        "disk_isolated_sandbox_smoke",
        "disk_isolated_vcs_integrity",
        "harness_container_sandbox_e2e",
    ],
    "sandbox-runtime-resource-governance": [
        "resource_governance_systemd_e2e",
    ],
    "sandbox-runtime-memory-leak": [
        "memory_leak_e2e",
    ],
}

CTX_HTTP_MANUAL_ONLY_TESTS = [
    "attachments_demo_react",
]

CTX_HTTP_INTEGRATION_SOURCE_DEPS = {
    "acp_crp_bridge_tokens_e2e": [
        "//core/crates/ctx-settings-model:lib",
        "//core/crates/ctx-settings-service:lib",
    ],
    "harness_container_sandbox_e2e": [
        "//core/crates/ctx-settings-model:lib",
        "//core/crates/ctx-settings-service:lib",
    ],
    "memory_leak_e2e": [
        "//core/crates/ctx-settings-model:lib",
    ],
    "provider_target_scoped_installs": [
        "//core/crates/ctx-settings-model:lib",
        "//core/crates/ctx-settings-service:lib",
    ],
    "resource_governance_systemd_e2e": [
        "//core/crates/ctx-settings-model:lib",
    ],
    "workspace_attachments_local_canonical": [
        "//core/crates/ctx-settings-model:lib",
        "//core/crates/ctx-settings-service:lib",
    ],
    "workspace_execution_config_http": [
        "//core/crates/ctx-settings-model:lib",
    ],
    "workspace_runtime_crash_recovery": [
        "//core/crates/ctx-settings-model:lib",
    ],
    "title_generation_local": [
        "//core/crates/ctx-settings-model:lib",
    ],
    "title_generation_local_e2e": [
        "//core/crates/ctx-settings-model:lib",
    ],
}

CTX_HTTP_INTEGRATION_CRATE_FEATURES = {
    "replay_properties": ["property_tests"],
}

CTX_HTTP_SUITE_EXTRA_TARGETS = {
    "scheduler-runtime": [
        "//core/crates/ctx-daemon:unit_tests_scheduler",
    ],
}

CTX_HTTP_TIMING_SENSITIVE_INTEGRATION_TESTS = [
    "provider_worker_reaping_offline",
    "provider_target_scoped_installs",
    "subagent_mcp_http",
    "subscription_accounts_api",
    "worktree_vcs_snapshot",
    "workspace_active_snapshot_http",
]

CTX_HTTP_CUSTOM_INTEGRATION_TARGETS = {
    "workspace_active_snapshot_http": {
        "source": "workspace_active_snapshot_http",
        "args": [
            "--skip",
            "workspace_vcs_stream_repeat_subscribe_preserves_ready_worktree_vcs_state",
            "--skip",
            "workspace_vcs_stream_subscribe_does_not_reemit_when_worktree_vcs_is_already_computing",
            "--skip",
            "worktree_vcs_summary_refresh_reloads_live_inventory_before_ready_publish",
        ],
        "timeout": "long",
    },
    "workspace_active_snapshot_http_workspace_vcs_stream_repeat_subscribe_preserves_ready_worktree_vcs_state": {
        "source": "workspace_active_snapshot_http",
        "args": ["--exact", "workspace_vcs_stream_repeat_subscribe_preserves_ready_worktree_vcs_state"],
        "timeout": "long",
    },
    "workspace_active_snapshot_http_workspace_vcs_stream_subscribe_does_not_reemit_when_worktree_vcs_is_already_computing": {
        "source": "workspace_active_snapshot_http",
        "args": ["--exact", "workspace_vcs_stream_subscribe_does_not_reemit_when_worktree_vcs_is_already_computing"],
        "timeout": "long",
    },
    "workspace_active_snapshot_http_worktree_vcs_summary_refresh_reloads_live_inventory_before_ready_publish": {
        "source": "workspace_active_snapshot_http",
        "args": ["--exact", "worktree_vcs_summary_refresh_reloads_live_inventory_before_ready_publish"],
        "timeout": "long",
    },
    "message_idempotency_post_message_idempotent_same_payload": {
        "source": "message_idempotency",
        "args": ["--exact", "post_message_idempotent_same_payload"],
        "timeout": "long",
    },
    "message_idempotency_post_message_idempotent_conflict_on_change": {
        "source": "message_idempotency",
        "args": ["--exact", "post_message_idempotent_conflict_on_change"],
        "timeout": "long",
    },
    "global_id_routing_http_artifact_route_is_session_scoped": {
        "source": "global_id_routing_http",
        "args": ["--exact", "artifact_route_is_session_scoped"],
        "timeout": "long",
    },
    "global_id_routing_http_quicktime_artifact_upload_is_accepted": {
        "source": "global_id_routing_http",
        "args": ["--exact", "quicktime_artifact_upload_is_accepted"],
        "timeout": "long",
    },
    "global_id_routing_http_message_delete_route_is_session_scoped": {
        "source": "global_id_routing_http",
        "args": ["--exact", "message_delete_route_is_session_scoped"],
        "timeout": "long",
    },
    "global_id_routing_http_subagent_invocation_route_is_session_scoped": {
        "source": "global_id_routing_http",
        "args": ["--exact", "subagent_invocation_route_is_session_scoped"],
        "timeout": "eternal",
    },
    "workspace_stream_no_gaps_under_activity": {
        "source": "workspace_stream_no_gaps_under_activity",
        "args": [],
        "timeout": "long",
    },
    "global_id_routing_http": {
        "source": "global_id_routing_http",
        "args": [],
        "timeout": "eternal",
    },
    "image_attachments_http_e2e": {
        "source": "image_attachments_http_e2e",
        "args": [],
        "timeout": "eternal",
    },
    "acp_target_scoped_status": {
        "source": "acp_target_scoped_status",
        "args": [],
        "timeout": "long",
    },
    "codex_host_import_api": {
        "source": "codex_host_import_api",
        "args": [],
        "timeout": "long",
    },
    "codex_login_callback_api": {
        "source": "codex_login_callback_api",
        "args": [],
        "timeout": "long",
    },
    "install_start_contract": {
        "source": "install_start_contract",
        "args": [],
        "timeout": "long",
    },
    "provider_current_ctx_version_regressions": {
        "source": "provider_current_ctx_version_regressions",
        "args": [],
        "timeout": "long",
    },
    "session_model_api": {
        "source": "session_model_api",
        "args": [],
        "timeout": "eternal",
    },
    "workspace_provider_model_preferences_http": {
        "source": "workspace_provider_model_preferences_http",
        "args": [],
        "timeout": "eternal",
    },
    "provider_probe_runtime_env": {
        "source": "provider_probe_runtime_env",
        "args": [],
        "timeout": "long",
    },
    "provider_worker_reaping_offline": {
        "source": "provider_worker_reaping_offline",
        "args": [],
        "timeout": "long",
    },
    "terminal_workspace_stream_separation": {
        "source": "terminal_workspace_stream_separation",
        "args": [],
        "timeout": "eternal",
    },
    "subagent_mcp_http": {
        "source": "subagent_mcp_http",
        "args": ["--skip", "archive_agent_reclaims_dedicated_child_worktree"],
        "timeout": "eternal",
    },
    "subagent_mcp_http_archive_agent_reclaims_dedicated_child_worktree": {
        "source": "subagent_mcp_http",
        "args": ["--exact", "archive_agent_reclaims_dedicated_child_worktree"],
        "timeout": "eternal",
    },
    "workspace_attachments_local_canonical": {
        "source": "workspace_attachments_local_canonical",
        "args": [],
        "timeout": "eternal",
    },
    "provider_scenarios_offline_crp_fixtures": {
        "source": "provider_scenarios_offline",
        "args": ["--exact", "provider_scenarios_offline_crp_fixtures"],
        "timeout": "long",
    },
    "provider_scenarios_offline_interleaved_assistant_tools_do_not_fragment_messages": {
        "source": "provider_scenarios_offline",
        "args": ["--exact", "provider_scenarios_offline_interleaved_assistant_tools_do_not_fragment_messages"],
        "timeout": "long",
    },
    "provider_scenarios_offline_crp_fixtures_persist_context_window_metrics": {
        "source": "provider_scenarios_offline",
        "args": ["--exact", "provider_scenarios_offline_crp_fixtures_persist_context_window_metrics"],
        "timeout": "long",
    },
    "turn_lifecycle_events": {
        "source": "turn_lifecycle_events",
        "args": [],
        "tags": ["exclusive"],
        "timeout": "eternal",
    },
}

CTX_HTTP_BAZEL_MANUAL_ONLY_TARGET = "manual-only"
CTX_HTTP_UNIT_TEST_HARNESS_NAME = "ctx_http_unit_test_harness"
CTX_HTTP_UNIT_TEST_WRAPPER = "tests/run_unit_test_harness.sh"

def _as_label(name):
    return ":" + name

def _integration_rustc_env(rustc_env, binary_rustc_env):
    merged = {}
    for key, value in rustc_env.items():
        merged[key] = value
    for key, value in binary_rustc_env.items():
        merged[key] = value
    return merged

def _declare_ctx_http_test(name, source_name, binary_data, binary_rustc_env, common_srcs, compile_data, data, deps, proc_macro_deps, rustc_env, test_args, timeout = None, extra_tags = None):
    tags = ["manual"] if name in CTX_HTTP_MANUAL_ONLY_TESTS else []
    if extra_tags:
        tags = tags + extra_tags
    if name in CTX_HTTP_TIMING_SENSITIVE_INTEGRATION_TESTS:
        tags.append("exclusive")
    test_deps = deps + CTX_HTTP_INTEGRATION_SOURCE_DEPS.get(source_name, [])
    test_data = data + binary_data.get(source_name, [])
    kwargs = {}
    if timeout != None:
        kwargs["timeout"] = timeout
    elif name in CTX_HTTP_TIMING_SENSITIVE_INTEGRATION_TESTS:
        kwargs["timeout"] = "long"
    crate_features = CTX_HTTP_INTEGRATION_CRATE_FEATURES.get(source_name)
    if crate_features:
        kwargs["crate_features"] = crate_features
    rust_test(
        name = name,
        crate_name = name,
        crate_root = "tests/{}.rs".format(source_name),
        srcs = ["tests/{}.rs".format(source_name)] + common_srcs,
        args = test_args,
        compile_data = compile_data,
        data = test_data,
        edition = "2021",
        rustc_env = _integration_rustc_env(rustc_env, binary_rustc_env.get(source_name, {})),
        deps = test_deps,
        proc_macro_deps = proc_macro_deps,
        tags = tags,
        **kwargs
    )

def declare_ctx_http_unit_test_harness(compile_data, data, deps, proc_macro_deps):
    rust_test(
        name = CTX_HTTP_UNIT_TEST_HARNESS_NAME,
        crate = ":lib_test_support",
        args = ["--list"],
        compile_data = compile_data,
        data = data,
        deps = deps,
        proc_macro_deps = proc_macro_deps,
        tags = ["manual"],
        timeout = "long",
    )

def declare_ctx_http_filtered_unit_test(name, args, compile_data = None, data = None, deps = None, proc_macro_deps = None, timeout = None, tags = None, crate = None):
    kwargs = {}
    kwargs["timeout"] = timeout if timeout != None else "long"
    if tags != None:
        kwargs["tags"] = tags
    sh_test(
        name = name,
        srcs = [CTX_HTTP_UNIT_TEST_WRAPPER],
        args = ["$(rootpath :{})".format(CTX_HTTP_UNIT_TEST_HARNESS_NAME)] + args,
        data = [":{}".format(CTX_HTTP_UNIT_TEST_HARNESS_NAME)] + (data if data != None else []),
        **kwargs
    )

def declare_ctx_http_rust_unit_test(name, args, compile_data, data, deps, proc_macro_deps, timeout = None, tags = None):
    declare_ctx_http_filtered_unit_test(
        name = name,
        args = args,
        compile_data = compile_data,
        data = data,
        deps = deps,
        proc_macro_deps = proc_macro_deps,
        timeout = timeout,
        tags = tags,
    )

def declare_ctx_http_integration_tests(binary_data, binary_rustc_env, common_srcs, compile_data, data, deps, proc_macro_deps, rustc_env, test_args):
    declared = {}
    for suite_name in CTX_HTTP_SUITE_ORDER:
        test_labels = []
        for test_name in CTX_HTTP_SUITE_TESTS[suite_name]:
            if test_name not in declared:
                custom = CTX_HTTP_CUSTOM_INTEGRATION_TARGETS.get(test_name)
                _declare_ctx_http_test(
                    name = test_name,
                    source_name = custom["source"] if custom else test_name,
                    binary_data = binary_data,
                    binary_rustc_env = binary_rustc_env,
                    common_srcs = common_srcs,
                    compile_data = compile_data,
                    data = data,
                    deps = deps,
                    proc_macro_deps = proc_macro_deps,
                    rustc_env = rustc_env,
                    test_args = test_args + custom["args"] if custom else test_args,
                    timeout = custom.get("timeout") if custom else None,
                    extra_tags = custom.get("tags") if custom else None,
                )
                declared[test_name] = True
            test_labels.append(_as_label(test_name))
        test_labels.extend(CTX_HTTP_SUITE_EXTRA_TARGETS.get(suite_name, []))
        native.test_suite(
            name = suite_name,
            tests = test_labels,
        )

    manual_test_labels = []
    for test_name in CTX_HTTP_MANUAL_ONLY_TESTS:
        if test_name not in declared:
            _declare_ctx_http_test(
                name = test_name,
                source_name = test_name,
                binary_data = binary_data,
                binary_rustc_env = binary_rustc_env,
                common_srcs = common_srcs,
                compile_data = compile_data,
                data = data,
                deps = deps,
                proc_macro_deps = proc_macro_deps,
                rustc_env = rustc_env,
                test_args = test_args,
                timeout = None,
            )
            declared[test_name] = True
        manual_test_labels.append(_as_label(test_name))

    native.test_suite(
        name = CTX_HTTP_BAZEL_MANUAL_ONLY_TARGET,
        tags = ["manual"],
        tests = manual_test_labels,
    )

    native.test_suite(
        name = "all",
        tests = [_as_label("base")] + [_as_label(suite_name) for suite_name in CTX_HTTP_SUITE_ORDER],
    )

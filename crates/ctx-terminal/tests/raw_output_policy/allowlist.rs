use super::{AllowEntry, OutputClass, Primitive, TestOwner};

// This list is intentionally exact. Do not add a directory-wide or file-wide
// exception. Every entry expands to one normalized source statement plus its
// output contract and test owner. A false positive must be fixed by narrowing
// the detector with a focused scanner test, not by broadening this list.
macro_rules! allow {
    ($path:expr, $fingerprint:literal, $primitive:ident, $class:ident, $why:expr, $owner:expr) => {
        AllowEntry {
            path: $path,
            fingerprint: $fingerprint,
            primitive: Primitive::$primitive,
            class: OutputClass::$class,
            rationale: $why,
            owning_test: $owner,
        }
    };
}

const GATE: TestOwner = TestOwner::behavioral(
    "tests/raw_output_policy.rs::production_raw_output_inventory_is_closed",
    &[
        "build.rs",
        "crates/ctx-semantic-model/build.rs",
        "crates/ctx-upgrade-engine/build.rs",
    ],
    &["compare_policy", "scan_package", "is_closed"],
);
const UNIT: TestOwner = TestOwner::behavioral(
    "crates/ctx-terminal/src/ui/tests.rs::ui_owns_independent_injectable_streams_and_capabilities",
    &[
        "src/dispatch.rs",
        "src/main.rs",
        "crates/ctx-terminal/src/output.rs",
        "src/release_build_identity.rs",
        "src/transcript.rs",
        "crates/ctx-terminal/src/ui/",
    ],
    &[
        "Ui",
        "with_writers",
        "write_stdout",
        "write_stderr",
        "flush",
    ],
);
const ANALYTICS: TestOwner = TestOwner::behavioral(
    "crates/ctx-client-observability/src/analytics/sender.rs::capability_ack_tracks_the_snapshot_bearing_chunk_not_later_chunks",
    &["src/analytics.rs"],
    &["post_event_chunks", "failure_on_post", "is_ok"],
);
const LIVE_OUTPUT: TestOwner = TestOwner::behavioral(
    "crates/ctx-terminal/src/ui/writer/tests.rs::live_controller_bytes_cover_first_grow_shrink_and_final_frames",
    &["crates/ctx-terminal/src/ui/writer.rs"],
    &["LiveOutput", "write_frame", "assert_eq"],
);
const LIVE_RESIZE: TestOwner = TestOwner::behavioral(
    "crates/ctx-terminal/src/ui/writer/tests.rs::resize_invalidates_wrapped_rows_and_restores_cursor_after_height_change",
    &["crates/ctx-terminal/src/ui/writer.rs"],
    &["LiveOutput", "render_frame", "shrink_repaint", "SENTINEL"],
);
const PLAIN_REFRESH_PROGRESS: TestOwner = TestOwner::behavioral(
    "crates/ctx-terminal/src/progress/tests.rs::plain_refresh_progress_is_the_stable_live_document_without_internal_routes",
    &["crates/ctx-terminal/src/progress.rs"],
    &["ProgressMode", "Plain", "refresh_progress", "assert_eq"],
);
const CALLOUT_PLAIN_MESSAGE: TestOwner = TestOwner::behavioral(
    "crates/ctx-terminal/src/progress/tests/notice_tests.rs::structured_callout_is_ordered_after_plain_progress_and_stays_structured_in_json",
    &[
        "crates/ctx-terminal/src/progress.rs",
        "crates/ctx-terminal/src/ui/components/callout.rs",
    ],
    &["CalloutPresentation", "plain_message", "assert_eq"],
);
const APPEND_OUTPUT: TestOwner = TestOwner::behavioral(
    "crates/ctx-terminal/src/ui/writer/tests.rs::append_controller_writes_documents_and_lines_exactly",
    &["crates/ctx-terminal/src/ui/writer.rs"],
    &["LiveOutput", "write_document", "write_line", "assert_eq"],
);
const SOURCE_INDEX_MACHINE_ERROR: TestOwner = TestOwner::behavioral(
    "crates/ctx-history-cli/src/source_index/tests/recovery.rs::show_and_search_generation_races_use_the_stable_retryable_json_envelope",
    &["crates/ctx-history-cli/src/source_index/shared.rs"],
    &["render_show_error", "render_search_error", "from_str"],
);
const SEARCH_DISPATCH_MACHINE_ERROR: TestOwner = TestOwner::behavioral(
    "src/dispatch/finalization.rs::search_machine_error_uses_the_ui_writer_and_propagates_failure",
    &["src/dispatch.rs"],
    &["write_machine_error", "stderr_copy", "unwrap_err"],
);
const SOURCE_INDEX_STREAM: TestOwner = TestOwner::behavioral(
    "crates/ctx-history-cli/src/source_index/tests/additional.rs::unbounded_cli_show_streams_valid_json_beyond_4096_events_in_order",
    &["crates/ctx-history-cli/src/source_index/show.rs"],
    &["stream_cli_session", "events_returned", "from_slice"],
);
const SOURCE_INDEX_HUMAN_STREAM: TestOwner = TestOwner::behavioral(
    "crates/ctx-history-cli/src/source_index/tests/additional.rs::human_cli_show_stream_renders_header_events_empty_and_truncation",
    &["crates/ctx-history-cli/src/source_index/show.rs"],
    &[
        "stream_cli_session",
        "OutputFormat::Text",
        "Transcript is truncated.",
        "No transcript events.",
    ],
);
const HISTORY_CLI_TERMINAL_PORT: TestOwner = TestOwner::behavioral(
    "crates/ctx-history-cli/src/ports.rs::terminal_port_preserves_selected_stream_bytes",
    &["crates/ctx-history-cli/src/ports.rs"],
    &["TerminalPort", "OutputStream", "write", "assert_eq"],
);
const LIST_EVENTS_STREAM: TestOwner = TestOwner::behavioral(
    "crates/ctx-history-cli/src/list_events/tests.rs::jsonl_flushes_each_event_before_fetching_the_next_page_and_completes_once",
    &["crates/ctx-history-cli/src/list_events.rs"],
    &["write_jsonl_pages", "flush_offsets", "completion"],
);
const LIST_EVENTS_ERROR: TestOwner = TestOwner::behavioral(
    "crates/ctx-history-cli/src/list_events/tests.rs::run_writes_typed_resource_errors_only_to_the_selected_machine_stderr",
    &["crates/ctx-history-cli/src/list_events.rs"],
    &["run", "stderr_copy", "from_slice"],
);
const STATUS_FAILURES: TestOwner = TestOwner::behavioral(
    "crates/ctx-cli-presentation/src/commands/status_usage.rs::status_failures_have_exact_machine_and_human_presentation",
    &["src/commands/status/usage.rs"],
    &[
        "malformed_status_config_json",
        "removed_cloud_config_json",
        "assert_eq",
    ],
);
const STATUS_ACTION_ERROR: TestOwner = TestOwner::behavioral(
    "crates/ctx-cli-presentation/src/commands/status_usage.rs::usage_machine_receipts_keep_the_exact_public_schema",
    &["src/commands/status/usage.rs"],
    &["usage_action_error_json", "UsageStatusMode", "Reset", "assert_eq"],
);
const MCP_SERVER: TestOwner = TestOwner::behavioral(
    "crates/ctx-agent-application/src/mcp/tests.rs::response_flush_precedes_the_one_usage_commit_and_post_flush_telemetry",
    &[
        "src/mcp.rs",
        "crates/ctx-agent-application/src/mcp/mod.rs",
    ],
        &["run_one_response", "flushed_at", "recorded_at"],
);
const HOSTED_TRANSACTION_RECEIPT: TestOwner = TestOwner::behavioral(
    "crates/ctx-upgrade-engine/src/upgrade/install/hosted_transaction/tests.rs::hosted_transaction_receipts_keep_the_stable_machine_schema",
    &["crates/ctx-upgrade-engine/src/upgrade/install/hosted_transaction.rs"],
    &["install_receipt", "uninstall_receipt", "install_value"],
);
const WINDOWS_READINESS: TestOwner = TestOwner::behavioral(
    "crates/ctx-upgrade-engine/src/upgrade/install/transaction/windows/tests.rs::readiness_receipt_is_exact_and_bounded",
    &["crates/ctx-upgrade-engine/src/upgrade/install/transaction/windows/helper.rs"],
    &["ready_receipt", "validate_ready_receipt"],
);
const DISPATCH_MACHINE_ERROR: TestOwner = TestOwner::behavioral(
    "src/dispatch/tests.rs::forced_color_never_decorates_generic_machine_mode_errors",
    &["src/dispatch.rs"],
    &["render_generic_command_error", "machine_stderr", "contains"],
);
const CLAP_OUTPUT: TestOwner = TestOwner::behavioral(
    "src/dispatch/tests.rs::clap_value_errors_use_the_selected_stderr_stream_with_contextual_usage",
    &["src/dispatch.rs"],
    &["write_clap_output", "contains", "rendered"],
);
const COMPANION_ROUTE: TestOwner = TestOwner::behavioral(
    "src/companion/contract_tests.rs::subcommand_help_and_help_alias_route_to_the_companion",
    &["src/companion.rs"],
    &["paid_family_arguments", "assert_eq"],
);
const CORE_CAPABILITY_RESPONSE: TestOwner = TestOwner::behavioral(
    "src/core_capability/contract_tests.rs::capability_response_is_one_exact_flushed_json_frame",
    &[
        "src/core_capability.rs",
        "src/core_capability/hosted_pair_install.rs",
    ],
    &["write_response_frame", "assert_eq"],
);
const HOSTED_PAIR_INSTALL_ERROR: TestOwner = TestOwner::behavioral(
    "src/core_capability/contract_tests.rs::only_the_exact_hidden_argv_is_intercepted",
    &["src/core_capability.rs"],
    &["intercept", "HOSTED_PAIR_INSTALL_INVOCATION", "is_none"],
);
const CARGO_DIRECTIVE: &str = "Cargo build-script protocol directive";
const JSON_PROTOCOL: &str = "documented JSON or JSONL machine-output contract";
const TEXT_PROTOCOL: &str = "documented plain-text machine-output contract";
const DEBUG_DIAGNOSTIC: &str =
    "CTX_ANALYTICS_DEBUG-only delivery-failure diagnostic; the owner injects and asserts the post failure path";
const TERMINAL_PROBE: &str = "terminal capability probe; emits no bytes";
const RAW_INFRASTRUCTURE: &str = "central raw-output infrastructure seam";
const UI_INFRASTRUCTURE: &str = "central Ui/Document rendering infrastructure seam";
const SPECIALIZED_STREAM: &str = "specialized streaming renderer owns framing and writes";

const CLI_BUILD: &str = "build.rs";
const MODEL_BUILD: &str = "crates/ctx-semantic-model/build.rs";
const ENGINE_BUILD: &str = "crates/ctx-upgrade-engine/build.rs";
const ANALYTICS_SENDER: &str = "src/analytics.rs";
const COMPANION: &str = "src/companion.rs";
const CORE_CAPABILITY: &str = "src/core_capability.rs";
const HOSTED_PAIR_INSTALL: &str = "src/core_capability/hosted_pair_install.rs";
const LIST_EVENTS: &str = "crates/ctx-history-cli/src/list_events.rs";
const SOURCE_INDEX_SHOW: &str = "crates/ctx-history-cli/src/source_index/show.rs";
const SOURCE_INDEX_SHARED: &str = "crates/ctx-history-cli/src/source_index/shared.rs";
const STATUS_USAGE: &str = "src/commands/status/usage.rs";
const DISPATCH: &str = "src/dispatch.rs";
const MAIN: &str = "src/main.rs";
const MCP_MODULE: &str = "src/mcp.rs";
const OUTPUT: &str = "crates/ctx-terminal/src/output.rs";
const RELEASE_IDENTITY: &str = "src/release_build_identity.rs";
const UI_DOCUMENT: &str = "crates/ctx-terminal/src/ui/document.rs";
const UI_CALLOUT: &str = "crates/ctx-terminal/src/ui/components/callout.rs";
const UI_MODULE: &str = "crates/ctx-terminal/src/ui/mod.rs";
const UI_WRITER: &str = "crates/ctx-terminal/src/ui/writer.rs";
const TERMINAL_PROGRESS: &str = "crates/ctx-terminal/src/progress.rs";
const HOSTED_TRANSACTION: &str =
    "crates/ctx-upgrade-engine/src/upgrade/install/hosted_transaction.rs";
const WINDOWS_HELPER: &str =
    "crates/ctx-upgrade-engine/src/upgrade/install/transaction/windows/helper.rs";
const PRESENTATION_INDEX: &str = "crates/ctx-cli-presentation/src/commands/index.rs";
const PRESENTATION_DOCS: &str = "crates/ctx-cli-presentation/src/docs.rs";
const SKILL_SELECTION: &str = "crates/ctx-cli-presentation/src/skill/selection.rs";

const INDEX_WAIT_OUTPUT: TestOwner = TestOwner::behavioral(
    "crates/ctx-cli-presentation/src/commands/index_tests.rs::wait_human_output_prints_a_changed_final_snapshot",
    &[PRESENTATION_INDEX],
    &["IndexWaitHumanOutput", "assert_eq", "rendered"],
);
const DOCS_OUTPUT: TestOwner = TestOwner::behavioral(
    "crates/ctx-cli-presentation/src/docs/ui_tests.rs::docs_machine_and_plain_branches_write_exact_selected_stdout_protocols",
    &[PRESENTATION_DOCS],
    &["list_docs", "search_docs", "show_doc", "man_docs", "assert_eq"],
);
const SKILL_INTERACTIVE_PROMPT: TestOwner = TestOwner::behavioral(
    "crates/ctx-cli-presentation/src/skill/selection.rs::prompt_for_agents_writes_exact_selected_stderr_protocol",
    &[SKILL_SELECTION],
    &["prompt_for_agents", "stderr", "assert_eq"],
);
const SKILL_INTERACTIVE_PICKER: TestOwner = TestOwner::behavioral(
    "crates/ctx-cli-presentation/src/skill/selection.rs::prompt_for_agents_with_io_retries_on_stderr_and_returns_the_selected_agents",
    &[SKILL_SELECTION],
    &["prompt_for_agents_with_io", "rendered", "assert_eq"],
);
const SKILL_INTERACTIVE_CAPABILITY: TestOwner = TestOwner::behavioral(
    "crates/ctx-cli-presentation/src/skill/selection.rs::can_prompt_rejects_asymmetric_tty_streams",
    &[SKILL_SELECTION],
    &["can_prompt", "is_terminal", "assert_eq"],
);
const UI_MACHINE_BYTES: TestOwner = TestOwner::behavioral(
    "crates/ctx-terminal/src/ui/tests.rs::ui_writes_exact_framed_machine_bytes_to_the_selected_stream",
    &[UI_WRITER],
    &["write_stdout_bytes", "write_stderr_bytes", "assert_eq"],
);
pub(super) const ALLOWLIST: &[AllowEntry] = &[
    allow!(
        CLI_BUILD,
        "main#1@3d618d0d6e1305c1",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        CLI_BUILD,
        "main#2@cfdf571c6929ef24",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        CLI_BUILD,
        "main#3@0acadf523050c898",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        CLI_BUILD,
        "main#4@17e10d71b62c3756",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        MODEL_BUILD,
        "main#1@0947514f54f72ef8",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        MODEL_BUILD,
        "main#2@90a37aff9068bc84",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        ENGINE_BUILD,
        "main#1@3d618d0d6e1305c1",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        ENGINE_BUILD,
        "main#2@45d34d119bef8d7e",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        ENGINE_BUILD,
        "main#3@17e10d71b62c3756",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        ANALYTICS_SENDER,
        "send_batch#1@6be4e73f7b0749ad",
        PrintMacro,
        JustifiedPlainHuman,
        DEBUG_DIAGNOSTIC,
        ANALYTICS
    ),
    allow!(
        COMPANION,
        "write_companion_stderr#1@a2a910058f52867c",
        DirectWrite,
        Infrastructure,
        "bounded opaque companion stderr forwarding",
        COMPANION_ROUTE
    ),
    allow!(
        HOSTED_PAIR_INSTALL,
        "run#1@1ef2a6ac56c71cd9",
        StdoutConstructor,
        Infrastructure,
        "bounded Core-capability response stream",
        CORE_CAPABILITY_RESPONSE
    ),
    allow!(
        CORE_CAPABILITY,
        "run#1@7bc10d6ee0b09708",
        StdoutConstructor,
        Infrastructure,
        "bounded Core-capability response stream",
        CORE_CAPABILITY_RESPONSE
    ),
    allow!(
        CORE_CAPABILITY,
        "intercept#1@b829ec991f891e23",
        PrintMacro,
        JustifiedPlainHuman,
        "bounded hidden hosted-install failure diagnostic",
        HOSTED_PAIR_INSTALL_ERROR
    ),
    allow!(
        CORE_CAPABILITY,
        "write_response_frame#1@256ff91bbfae0edf",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        CORE_CAPABILITY_RESPONSE
    ),
    allow!(
        CORE_CAPABILITY,
        "write_response_frame#2@a125c974b59a63d4",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        CORE_CAPABILITY_RESPONSE
    ),
    allow!(
        COMPANION,
        "write_companion_stderr#1@a2a910058f52867c",
        StderrConstructor,
        Infrastructure,
        "bounded opaque companion stderr forwarding",
        COMPANION_ROUTE
    ),
    allow!(
        COMPANION,
        "write_companion_stderr#2@796855835a366312",
        StderrConstructor,
        Infrastructure,
        "flush for bounded opaque companion stderr forwarding",
        COMPANION_ROUTE
    ),
    allow!(
        COMPANION,
        "write_cli_launch_error#1@3f1d569965a6609b",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        COMPANION_ROUTE
    ),
    allow!(
        COMPANION,
        "write_cli_launch_error#1@8d1da5f184bab174",
        StderrConstructor,
        MachineProtocol,
        JSON_PROTOCOL,
        COMPANION_ROUTE
    ),
    allow!(
        LIST_EVENTS,
        "run#1@75fb49cf76ccc9aa",
        UiRawWriter,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIST_EVENTS_STREAM
    ),
    allow!(
        LIST_EVENTS,
        "run#1@4304839f9eecc9a1",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        LIST_EVENTS_ERROR
    ),
    allow!(
        LIST_EVENTS,
        "run#2@4fcd3f430927aff6",
        UiRawWriter,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        LIST_EVENTS_ERROR
    ),
    allow!(
        LIST_EVENTS,
        "execute#1@9e7b8ecbd7f07cb4",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        LIST_EVENTS_STREAM
    ),
    allow!(
        SOURCE_INDEX_SHOW,
        "stream_cli_session#1@726192492b47f797",
        UiRawWriter,
        Infrastructure,
        SPECIALIZED_STREAM,
        SOURCE_INDEX_STREAM
    ),
    allow!(
        SOURCE_INDEX_SHOW,
        "emit#1@e00603b31705f733",
        DocumentRender,
        JustifiedPlainHuman,
        "streamed human show events use the shared terminal document renderer",
        SOURCE_INDEX_HUMAN_STREAM
    ),
    allow!(
        SOURCE_INDEX_SHOW,
        "finish#1@83de2ef9ce802211",
        DocumentRender,
        JustifiedPlainHuman,
        "streamed empty human show results use the shared terminal document renderer",
        SOURCE_INDEX_HUMAN_STREAM
    ),
    allow!(
        SOURCE_INDEX_SHOW,
        "finish#2@bc572009073a0869",
        DocumentRender,
        JustifiedPlainHuman,
        "streamed truncated human show results use the shared terminal document renderer",
        SOURCE_INDEX_HUMAN_STREAM
    ),
    allow!(
        SOURCE_INDEX_SHOW,
        "write_header#1@311e92a0e876bc18",
        DocumentRender,
        JustifiedPlainHuman,
        "streamed human show headers use the shared terminal document renderer",
        SOURCE_INDEX_HUMAN_STREAM
    ),
    allow!(
        SOURCE_INDEX_SHARED,
        "render_active_generation_race#1@4f414268237841ec",
        UiRawWriter,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        SOURCE_INDEX_MACHINE_ERROR
    ),
    allow!(
        SOURCE_INDEX_SHARED,
        "render_active_generation_race#1@0caa61c845c0cfcb",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        SOURCE_INDEX_MACHINE_ERROR
    ),
    allow!(
        "crates/ctx-history-cli/src/ports.rs",
        "write#1@835e4aa3042c909d",
        DirectWrite,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        HISTORY_CLI_TERMINAL_PORT
    ),
    allow!(
        "crates/ctx-history-cli/src/ports.rs",
        "write#1@835e4aa3042c909d",
        UiRawWriter,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        HISTORY_CLI_TERMINAL_PORT
    ),
    allow!(
        "crates/ctx-history-cli/src/ports.rs",
        "write#2@054d53ac226176a9",
        DirectWrite,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        HISTORY_CLI_TERMINAL_PORT
    ),
    allow!(
        "crates/ctx-history-cli/src/ports.rs",
        "write#2@054d53ac226176a9",
        UiRawWriter,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        HISTORY_CLI_TERMINAL_PORT
    ),
    allow!(
        SOURCE_INDEX_SHOW,
        "run_show_inner#1@958205ea7e234562",
        UiRawWriter,
        Infrastructure,
        SPECIALIZED_STREAM,
        SOURCE_INDEX_STREAM
    ),
    allow!(
        STATUS_USAGE,
        "malformed_config_failure#1@99c5d29aec7687ea",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS_FAILURES
    ),
    allow!(
        STATUS_USAGE,
        "removed_cloud_config_failure#1@f6d3abf00cb3c8c8",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS_FAILURES
    ),
    allow!(
        STATUS_USAGE,
        "usage_action_failure#1@c224d57aeae379ca",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS_ACTION_ERROR
    ),
    allow!(
        DISPATCH,
        "render_generic_command_error#1@28450a09db65187b",
        UiRawWriter,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        DISPATCH_MACHINE_ERROR
    ),
    allow!(
        DISPATCH,
        "render_generic_command_error#1@9deb49b5ff3a1d05",
        DirectWrite,
        MachineProtocol,
        "generic machine-mode command error",
        DISPATCH_MACHINE_ERROR
    ),
    allow!(
        DISPATCH,
        "write_machine_error#1@1d0a3152e9359dd8",
        DirectWrite,
        MachineProtocol,
        "Search machine-mode terminal error with observable write failure",
        SEARCH_DISPATCH_MACHINE_ERROR
    ),
    allow!(
        DISPATCH,
        "write_machine_error#1@f6527bbd5cb1a91e",
        UiRawWriter,
        Infrastructure,
        "Search machine-mode error uses the selected Ui stderr writer",
        SEARCH_DISPATCH_MACHINE_ERROR
    ),
    allow!(
        DISPATCH,
        "write_machine_error#1@6141a6c820169db8",
        PrintMacro,
        MachineProtocol,
        "unchanged generic machine-mode command error",
        UNIT
    ),
    allow!(
        DISPATCH,
        "run#1@e980ea9ca2d818d3",
        PrintMacro,
        JustifiedPlainHuman,
        "last-resort plain fallback after structured stderr rendering itself fails",
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#1@611edc2f163d9789",
        StdoutConstructor,
        Infrastructure,
        "final process stream flush",
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#1@93f3ab5dd89cc205",
        StderrConstructor,
        Infrastructure,
        "final process stream flush",
        UNIT
    ),
    allow!(
        DISPATCH,
        "write_clap_output_with_line_ends#1@a446ef88164d6fbc",
        UiRawWriter,
        Infrastructure,
        "Clap owns parser/help framing while Ui owns the selected stream adapter",
        CLAP_OUTPUT
    ),
    allow!(
        DISPATCH,
        "write_clap_output_with_line_ends#1@b8f99857faf49882",
        DirectWrite,
        Infrastructure,
        "Clap owns parser/help framing while Ui owns the selected stream adapter",
        CLAP_OUTPUT
    ),
    allow!(
        DISPATCH,
        "write_clap_output_with_line_ends#2@17a79ef591783ebd",
        UiRawWriter,
        Infrastructure,
        "Clap owns parser/help framing while Ui owns the selected stream adapter",
        CLAP_OUTPUT
    ),
    allow!(
        DISPATCH,
        "write_clap_output_with_line_ends#2@737a4b274061e165",
        DirectWrite,
        Infrastructure,
        "Clap owns parser/help framing while Ui owns the selected stream adapter",
        CLAP_OUTPUT
    ),
    allow!(
        MAIN,
        "<module>#1@f3074dbc832134e6",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        MAIN,
        "<module>#2@3944b6a934da1cbe",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        MAIN,
        "<module>#3@442af30894812ce9",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        MAIN,
        "<module>#4@3b2708c9160a0fe9",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        MAIN,
        "<module>#5@247da9757849fb98",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        MCP_MODULE,
        "serve_stdio#1@57e14a2db2574477",
        StdoutConstructor,
        MachineProtocol,
        "MCP JSON-RPC transport owns stdout",
        MCP_SERVER
    ),
    allow!(
        OUTPUT,
        "<module>#1@f7d8c8de216a4a5f",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "<module>#2@04727f0794baa772",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "<module>#3@d400c41874c73f83",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "<module>#4@95a6dcc21d1c9339",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "stderr_writer#1@53302d94fe4bac6c",
        StderrConstructor,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write#1@b99da2fdfd7f5bb1",
        DirectWrite,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#1@1e41b5c64a14aad7",
        DirectWrite,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#2@1e41b5c64a14aad7",
        DirectWrite,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#3@1e41b5c64a14aad7",
        DirectWrite,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#4@1e41b5c64a14aad7",
        DirectWrite,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#1@305f719202f53e59",
        StdoutConstructor,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#1@d41377b1caba5395",
        StderrConstructor,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        RELEASE_IDENTITY,
        "print_if_requested#1@9143e700fc22b2e1",
        PrintMacro,
        MachineProtocol,
        TEXT_PROTOCOL,
        UNIT
    ),
    allow!(
        RELEASE_IDENTITY,
        "print_if_requested#2@031e61786d8747e3",
        PrintMacro,
        MachineProtocol,
        TEXT_PROTOCOL,
        UNIT
    ),
    allow!(
        RELEASE_IDENTITY,
        "print_if_requested#3@558f7fd3af60bc0f",
        PrintMacro,
        MachineProtocol,
        TEXT_PROTOCOL,
        UNIT
    ),
    allow!(
        UI_DOCUMENT,
        "<module>#1@c63b922269e6f670",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_DOCUMENT,
        "<module>#2@1f3e3a85e7741e93",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_CALLOUT,
        "plain_message#1@4adb1d63e3d6f7e3",
        DocumentRender,
        Infrastructure,
        "callout plain-message composition uses the shared terminal document renderer",
        CALLOUT_PLAIN_MESSAGE
    ),
    allow!(
        UI_MODULE,
        "canonical_human_output_bytes#1@172c103f5c672685",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        TERMINAL_PROGRESS,
        "write_progress#1@2ccc5c4ecdb1caa4",
        DocumentRender,
        JustifiedPlainHuman,
        "explicit plain refresh progress writes the canonical ANSI-free semantic document",
        PLAIN_REFRESH_PROGRESS
    ),
    allow!(
        UI_WRITER,
        "write_document#1@074a3456d7db4974",
        DirectWrite,
        Infrastructure,
        UI_INFRASTRUCTURE,
        APPEND_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_document#1@074a3456d7db4974",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        APPEND_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_line#1@9c23106aa6419c75",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        APPEND_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_line#2@26dbdba5809c8356",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        APPEND_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "render_frame#1@570278e406fabb4a",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        LIVE_RESIZE
    ),
    allow!(
        UI_WRITER,
        "render_frame#2@21157661e5d7d8c9",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        LIVE_RESIZE
    ),
    allow!(
        UI_WRITER,
        "write_rendered_frame#1@c3fea88d95687a11",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_rendered_frame#2@26dbdba5809c8356",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_rendered_frame#3@0ba0fd9d995d5136",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_rendered_frame#4@05c759eaddd10788",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "hide_cursor#1@f0842dd6b0821c92",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "repaint_changed_rows#1@ec7a11f231f69a0c",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "repaint_changed_rows#2@2aaf68bbaa8f574a",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "repaint_changed_rows#3@a4ce1b6d55888191",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "repaint_changed_rows#4@a5067d4393daff92",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "repaint_full_frame#1@25d91e214faa3726",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_RESIZE
    ),
    allow!(
        UI_WRITER,
        "repaint_full_frame#2@33d04ce985307180",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_RESIZE
    ),
    allow!(
        UI_WRITER,
        "repaint_full_frame#3@177ec66628e8b21e",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_RESIZE
    ),
    allow!(
        UI_WRITER,
        "finish_frame#1@17d0ff3cbb7a1828",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "restore_cursor#1@b60f68884f839ecf",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "restore_cursor_best_effort#1@4b8981666f3ce827",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_cursor_up#1@55e82b5760219112",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_cursor_down#1@4b32d88fa6ea6435",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "<module>#1@7446ea7cb0e48e89",
        UiRawWriter,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "<module>#1@37471075d156ae43",
        UiWriterInjection,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "<module>#2@b080a0d53dc5dc7c",
        UiRawWriter,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "stdio#1@57e14a2db2574477",
        StdoutConstructor,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "stdio#1@9d99ae52ba0872ab",
        StderrConstructor,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "stream_width#1@396d77072ca654e4",
        StdoutConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "stream_width#1@ce6feac81ccc3c46",
        StderrConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "write#1@cae68c8b3d8eadff",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "write#1@cae68c8b3d8eadff",
        DirectWrite,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        HOSTED_TRANSACTION,
        "install#1@953d9c197658b947",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        HOSTED_TRANSACTION_RECEIPT
    ),
    allow!(
        HOSTED_TRANSACTION,
        "print_uninstall_receipt#1@77194866b863aaa6",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        HOSTED_TRANSACTION_RECEIPT
    ),
    allow!(
        WINDOWS_HELPER,
        "write_ready#1@7412c9b708be0f94",
        StdoutConstructor,
        MachineProtocol,
        "writes protocol::ready_receipt verbatim; the owner asserts its exact bounded framing",
        WINDOWS_READINESS
    ),
    allow!(
        WINDOWS_HELPER,
        "write_ready#1@039a802fb7eff38d",
        DirectWrite,
        MachineProtocol,
        "writes protocol::ready_receipt verbatim; the owner asserts its exact bounded framing",
        WINDOWS_READINESS
    ),
    allow!(
        PRESENTATION_INDEX,
        "render#1@257ffe0fafbffd46",
        DocumentRender,
        Infrastructure,
        "deduplicates the final interactive index frame before Ui writes the document",
        INDEX_WAIT_OUTPUT
    ),
    allow!(
        PRESENTATION_DOCS,
        "man_docs#1@f687e712c7a89bd2",
        DocumentRender,
        Infrastructure,
        "measures the generated-man confirmation document before Ui writes it",
        DOCS_OUTPUT
    ),
    allow!(
        SKILL_SELECTION,
        "can_prompt#1@3f34b45c75c3f2ef",
        StderrConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        SKILL_INTERACTIVE_CAPABILITY
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents#1@f0b7df253176cfbb",
        UiRawWriter,
        Infrastructure,
        "interactive picker writes only through Ui's selected stderr stream",
        SKILL_INTERACTIVE_PROMPT
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents_with_io#1@47a2606e6662bb08",
        DirectWrite,
        JustifiedPlainHuman,
        "interactive picker prompt written to the measured stderr seam",
        SKILL_INTERACTIVE_PICKER
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents_with_io#2@c6a8085b6f4a378b",
        DirectWrite,
        JustifiedPlainHuman,
        "interactive picker prompt written to the measured stderr seam",
        SKILL_INTERACTIVE_PICKER
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents_with_io#3@2aff417542e17806",
        DirectWrite,
        JustifiedPlainHuman,
        "interactive picker validation written to the measured stderr seam",
        SKILL_INTERACTIVE_PICKER
    ),
    allow!(
        UI_WRITER,
        "write_stdout_bytes#1@640d2d4192c99163",
        DirectWrite,
        Infrastructure,
        "Ui owns selected-stream delivery for already-framed machine and plain-text protocols",
        UI_MACHINE_BYTES
    ),
    allow!(
        UI_WRITER,
        "write_stderr_bytes#1@18df0e7444fe5246",
        DirectWrite,
        Infrastructure,
        "Ui owns selected-stream delivery for already-framed machine and plain-text protocols",
        UI_MACHINE_BYTES
    ),
];

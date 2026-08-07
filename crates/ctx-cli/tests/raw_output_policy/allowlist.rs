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
    &["build.rs", "crates/ctx-semantic-model/build.rs"],
    &["compare_policy", "scan_package", "is_closed"],
);
const UNIT: TestOwner = TestOwner::behavioral(
    "src/ui/tests.rs::ui_owns_independent_injectable_streams_and_capabilities",
    &[
        "src/commands/blame.rs",
        "src/dispatch.rs",
        "src/main.rs",
        "src/output.rs",
        "src/release_build_identity.rs",
        "src/transcript.rs",
        "src/ui/",
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
    "src/analytics/sender.rs::capability_ack_tracks_the_snapshot_bearing_chunk_not_later_chunks",
    &["src/analytics/sender.rs"],
    &["post_event_chunks", "failure_on_post", "is_ok"],
);
const LIVE_OUTPUT: TestOwner = TestOwner::behavioral(
    "src/ui/writer.rs::live_controller_bytes_cover_first_grow_shrink_and_final_frames",
    &["src/ui/writer.rs"],
    &["LiveOutput", "write_frame", "assert_eq"],
);
const APPEND_OUTPUT: TestOwner = TestOwner::behavioral(
    "src/ui/writer.rs::append_controller_writes_documents_and_lines_exactly",
    &["src/ui/writer.rs"],
    &["LiveOutput", "write_document", "write_line", "assert_eq"],
);
const INDEX: TestOwner = TestOwner::behavioral(
    "src/commands/index_tests.rs::wait_human_output_prints_a_changed_final_snapshot",
    &["src/commands/index.rs"],
    &["IndexWaitHumanOutput", "print_final", "rendered"],
);
const SOURCE_INDEX_MACHINE_ERROR: TestOwner = TestOwner::behavioral(
    "src/commands/source_index/tests/recovery.rs::show_and_search_generation_races_use_the_stable_retryable_json_envelope",
    &["src/commands/source_index/shared.rs"],
    &["render_show_error", "render_search_error", "from_str"],
);
const SOURCE_INDEX_STREAM: TestOwner = TestOwner::behavioral(
    "src/commands/source_index/tests.rs::unbounded_cli_show_streams_valid_json_beyond_4096_events_in_order",
    &["src/commands/source_index/show.rs"],
    &["stream_cli_session", "events_returned", "from_slice"],
);
const LIST_EVENTS_STREAM: TestOwner = TestOwner::behavioral(
    "src/commands/list/events/tests.rs::jsonl_flushes_each_event_before_fetching_the_next_page_and_completes_once",
    &["src/commands/list/events.rs"],
    &["write_jsonl_pages", "flush_offsets", "completion"],
);
const LIST_EVENTS_ERROR: TestOwner = TestOwner::behavioral(
    "src/commands/list/events/tests.rs::run_writes_typed_resource_errors_only_to_the_selected_machine_stderr",
    &["src/commands/list/events.rs"],
    &["run", "stderr_copy", "from_slice"],
);
const STATS: TestOwner = TestOwner::behavioral(
    "src/commands/stats.rs::stats_plain_output_matches_ansi_stripped_output",
    &["src/commands/stats.rs"],
    &["render_stats_human", "strip_ansi", "render_plain"],
);
const STATUS: TestOwner = TestOwner::behavioral(
    "src/commands/status/usage.rs::usage_machine_receipts_keep_the_exact_public_schema",
    &["src/commands/status/usage.rs"],
    &["usage_action_json", "usage_action_error_json"],
);
const PUBLIC_HELP: TestOwner = TestOwner::behavioral(
    "src/docs.rs::docs_plain_output_matches_ansi_stripped_output",
    &["src/dispatch.rs", "src/docs.rs"],
    &["render_docs_list", "strip_ansi", "render_plain"],
);
const MCP: TestOwner = TestOwner::behavioral(
    "src/integrations/mcp/operation.rs::human_install_and_status_results_use_the_typed_ui",
    &["src/integrations/mcp/operation.rs"],
    &[
        "render_install_results",
        "render_status_results",
        "strip_ansi",
    ],
);
const MCP_SERVER: TestOwner = TestOwner::behavioral(
    "src/mcp/response_bound/tests.rs::final_mcp_serialization_is_bounded_after_json_expansion",
    &["src/mcp.rs"],
    &[
        "bound_show_mcp_response",
        "serialized_json_line_bytes",
        "TEST_OUTPUT_LIMIT",
    ],
);
const PRO: TestOwner = TestOwner::behavioral(
    "src/pro/referral.rs::success_json_preserves_the_exact_machine_contract",
    &["src/pro/"],
    &["create_output", "status_output"],
);
const PRO_MACHINE_ERROR: TestOwner = TestOwner::behavioral(
    "src/pro/tests.rs::stable_machine_errors_are_exact_json_without_untrusted_detail_or_ansi",
    &["src/dispatch.rs"],
    &["write_stable_error_json", "from_slice", "output"],
);
const PRO_BLAME_MACHINE_ERROR: TestOwner = TestOwner::behavioral(
    "src/pro/tests.rs::blame_machine_errors_are_typed_without_expanding_unrelated_pro_errors",
    &["src/dispatch.rs"],
    &["write_blame_error_json", "from_slice", "output"],
);
const SKILL: TestOwner = TestOwner::behavioral(
    "src/skill/install.rs::human_install_and_status_results_use_the_typed_ui",
    &["src/skill/"],
    &[
        "render_status_results",
        "render_install_results",
        "strip_ansi",
    ],
);
const SLASH: TestOwner = TestOwner::behavioral(
    "src/integrations/slash_commands/tests.rs::failed_target_details_and_recovery_are_written_to_stderr",
    &["src/integrations/slash_commands.rs"],
    &["Ui", "with_writers", "stderr_copy"],
);
const UPGRADE: TestOwner = TestOwner::behavioral(
    "src/upgrade/command/human.rs::automatic_mode_json_is_one_complete_machine_receipt",
    &[
        "src/upgrade/command/human.rs",
        "src/upgrade/command/status.rs",
    ],
    &["auto_mode_json"],
);
const HOSTED_TRANSACTION_RECEIPT: TestOwner = TestOwner::behavioral(
    "src/upgrade/install/hosted_transaction/tests.rs::hosted_transaction_receipts_keep_the_stable_machine_schema",
    &["src/upgrade/install/hosted_transaction.rs"],
    &["install_receipt", "uninstall_receipt", "install_value"],
);
const WINDOWS_READINESS: TestOwner = TestOwner::behavioral(
    "src/upgrade/install/transaction/windows/tests.rs::readiness_receipt_is_exact_and_bounded",
    &["src/upgrade/install/transaction/windows/helper.rs"],
    &["ready_receipt", "validate_ready_receipt"],
);
const DISPATCH_MACHINE_ERROR: TestOwner = TestOwner::behavioral(
    "src/dispatch.rs::forced_color_never_decorates_generic_machine_mode_errors",
    &["src/dispatch.rs"],
    &["render_generic_command_error", "machine_stderr", "contains"],
);
const CLAP_OUTPUT: TestOwner = TestOwner::behavioral(
    "src/dispatch.rs::clap_value_errors_use_the_selected_stderr_stream_with_contextual_usage",
    &["src/dispatch.rs"],
    &["write_clap_output", "contains", "rendered"],
);
const SKILL_PROMPT: TestOwner = TestOwner::behavioral(
    "src/skill/selection.rs::interactive_picker_prompt_is_explicit_and_actionable",
    &["src/skill/selection.rs"],
    &["picker_prompt_lines", "contains", "assert"],
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
const INTERACTIVE_PICKER: &str =
    "TTY-only interactive picker with explicit prompt framing and behavioral coverage";
const MACHINE_BODY: &str = "command emits a preformatted protocol body verbatim";

const CLI_BUILD: &str = "build.rs";
const MODEL_BUILD: &str = "crates/ctx-semantic-model/build.rs";
const ANALYTICS_SENDER: &str = "src/analytics/sender.rs";
const BLAME: &str = "src/commands/blame.rs";
const INDEX_COMMAND: &str = "src/commands/index.rs";
const LIST_EVENTS: &str = "src/commands/list/events.rs";
const SOURCE_INDEX_SHOW: &str = "src/commands/source_index/show.rs";
const SOURCE_INDEX_SHARED: &str = "src/commands/source_index/shared.rs";
const STATS_COMMAND: &str = "src/commands/stats.rs";
const STATUS_USAGE: &str = "src/commands/status/usage.rs";
const DISPATCH: &str = "src/dispatch.rs";
const DOCS: &str = "src/docs.rs";
const MCP_OPERATION: &str = "src/integrations/mcp/operation.rs";
const SLASH_COMMANDS: &str = "src/integrations/slash_commands.rs";
const MAIN: &str = "src/main.rs";
const MCP_MODULE: &str = "src/mcp.rs";
const OUTPUT: &str = "src/output.rs";
const PRO_LIFECYCLE: &str = "src/pro/lifecycle_commands.rs";
const PRO_SETUP_REPLAY: &str = "src/pro/lifecycle_commands/setup_replay.rs";
const PRO_UNINSTALL: &str = "src/pro/lifecycle_commands/uninstall.rs";
const PRO_PENDING: &str = "src/pro/pending_materialization.rs";
const PRO_REFERRAL: &str = "src/pro/referral.rs";
const PRO_RENDER: &str = "src/pro/render.rs";
const RELEASE_IDENTITY: &str = "src/release_build_identity.rs";
const SKILL_INSTALL: &str = "src/skill/install.rs";
const SKILL_SELECTION: &str = "src/skill/selection.rs";
const TRANSCRIPT: &str = "src/transcript.rs";
const UI_DOCUMENT: &str = "src/ui/document.rs";
const UI_MODULE: &str = "src/ui/mod.rs";
const UI_WRITER: &str = "src/ui/writer.rs";
const UPGRADE_HUMAN: &str = "src/upgrade/command/human.rs";
const UPGRADE_STATUS: &str = "src/upgrade/command/status.rs";
const HOSTED_TRANSACTION: &str = "src/upgrade/install/hosted_transaction.rs";
const WINDOWS_HELPER: &str = "src/upgrade/install/transaction/windows/helper.rs";

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
        "main#2@885cbe6e55e728c7",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        CLI_BUILD,
        "main#3@cfdf571c6929ef24",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        CLI_BUILD,
        "main#4@56760ef03ecfb333",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        CLI_BUILD,
        "main#5@0acadf523050c898",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        CLI_BUILD,
        "main#6@17e10d71b62c3756",
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
        ANALYTICS_SENDER,
        "send_batch#1@b98cc5c167ea7850",
        PrintMacro,
        JustifiedPlainHuman,
        DEBUG_DIAGNOSTIC,
        ANALYTICS
    ),
    allow!(
        BLAME,
        "run_with#1@862efb434b34b780",
        StdoutConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        UNIT
    ),
    allow!(
        BLAME,
        "run_with#1@862efb434b34b780",
        StderrConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        UNIT
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
        INDEX_COMMAND,
        "render#1@257ffe0fafbffd46",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        INDEX
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
        "execute#1@5f7779205b7edc27",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        LIST_EVENTS_STREAM
    ),
    allow!(
        LIST_EVENTS,
        "write_jsonl_pages#1@3e567be51ce81a14",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        LIST_EVENTS_STREAM
    ),
    allow!(
        LIST_EVENTS,
        "write_jsonl_pages#2@a58701092e0a06cc",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        LIST_EVENTS_STREAM
    ),
    allow!(
        SOURCE_INDEX_SHOW,
        "stream_cli_session#1@1b29ac2997edb0f6",
        UiRawWriter,
        Infrastructure,
        SPECIALIZED_STREAM,
        SOURCE_INDEX_STREAM
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
        STATS_COMMAND,
        "malformed_config_failure#1@5ad0927fe28eedf7",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATS
    ),
    allow!(
        STATUS_USAGE,
        "malformed_config_failure#1@3029b1d3e3586d6e",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS
    ),
    allow!(
        STATUS_USAGE,
        "removed_cloud_config_failure#1@d113765ce063d423",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS
    ),
    allow!(
        STATUS_USAGE,
        "usage_action_failure#1@c224d57aeae379ca",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS
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
        "render_stable_pro_error_json#1@cb8255dedc3256c6",
        OutputRawHelper,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO_MACHINE_ERROR
    ),
    allow!(
        DISPATCH,
        "render_blame_error_json#1@51617f6288b47ada",
        OutputRawHelper,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO_BLAME_MACHINE_ERROR
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
        "run_cli#1@2912ee43f3606c5f",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#2@64c8afd2f04c16a3",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#3@638b02f9a8248d06",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#4@1f99826fdc74e99b",
        PrintMacro,
        MachineProtocol,
        "generic machine-mode command error",
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
        DOCS,
        "list_docs#1@56ebd27b8774f7d6",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        PUBLIC_HELP
    ),
    allow!(
        DOCS,
        "man_docs#1@b952ea61fcba410a",
        PrintMacro,
        MachineProtocol,
        TEXT_PROTOCOL,
        PUBLIC_HELP
    ),
    allow!(
        DOCS,
        "man_docs#1@f687e712c7a89bd2",
        DocumentRender,
        Infrastructure,
        "measures generated manpage text without emitting it",
        PUBLIC_HELP
    ),
    allow!(
        DOCS,
        "search_docs#1@56ebd27b8774f7d6",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        PUBLIC_HELP
    ),
    allow!(
        DOCS,
        "show_doc#1@84b24dcf8cf5d1c5",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        PUBLIC_HELP
    ),
    allow!(
        MCP_OPERATION,
        "run_install#1@476a66834d6d3fcd",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        MCP
    ),
    allow!(
        MCP_OPERATION,
        "run_status#1@7e646bbdf5ed1d14",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        MCP
    ),
    allow!(
        SLASH_COMMANDS,
        "run_install#1@ae0247e1babb399b",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        SLASH
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
        MCP_MODULE,
        "serve_stdio_loop#1@0b1e1125489bffc5",
        DirectWrite,
        MachineProtocol,
        "MCP JSON-RPC response framing",
        MCP_SERVER
    ),
    allow!(
        OUTPUT,
        "<module>#1@c614d2315222fabf",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "<module>#2@7a634fe26bf78e12",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "<module>#3@469e3a07ae927e23",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "<module>#4@270a7b6cfa1ef559",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "print_json#1@75eb9112501374a4",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
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
        PRO_UNINSTALL,
        "emit_uninstall_result#1@79342920bb593fd0",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_LIFECYCLE,
        "run_manage_with_opener#1@79342920bb593fd0",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_SETUP_REPLAY,
        "write_setup_result#1@79342920bb593fd0",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_LIFECYCLE,
        "uninstall_data_disposition#1@833f888868adcb88",
        StderrConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        PRO
    ),
    allow!(
        PRO_PENDING,
        "defer_setup#1@a86ba39c2a24c819",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_REFERRAL,
        "run#1@c31c16c84a609c6f",
        StdoutConstructor,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_REFERRAL,
        "run#2@9332b03878d3276b",
        StdoutConstructor,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_REFERRAL,
        "run#3@c31c16c84a609c6f",
        StdoutConstructor,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_REFERRAL,
        "write_json#1@e4cc8f587660af31",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_REFERRAL,
        "write_cta#1@f53429f9d33162f3",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        PRO
    ),
    allow!(
        PRO_RENDER,
        "print_blame_result_with_context#1@8692675e47437164",
        UiRawWriter,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_RENDER,
        "print_blame_result_with_context#1@8692675e47437164",
        DirectWrite,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
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
        SKILL_INSTALL,
        "run_install#1@eea45ab138d6f8e8",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        SKILL
    ),
    allow!(
        SKILL_INSTALL,
        "run_status#1@80879dcf84b7283b",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        SKILL
    ),
    allow!(
        SKILL_SELECTION,
        "can_prompt#1@4290fcfa0041df77",
        StderrConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        SKILL_PROMPT
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents#1@4b2cc0708de7fe71",
        OutputRawHelper,
        InteractivePrompt,
        INTERACTIVE_PICKER,
        SKILL_PROMPT
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents#1@596d62caccf3cad9",
        DirectWrite,
        InteractivePrompt,
        INTERACTIVE_PICKER,
        SKILL_PROMPT
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents#2@c6a8085b6f4a378b",
        DirectWrite,
        InteractivePrompt,
        INTERACTIVE_PICKER,
        SKILL_PROMPT
    ),
    allow!(
        SKILL_SELECTION,
        "prompt_for_agents#3@2aff417542e17806",
        DirectWrite,
        InteractivePrompt,
        INTERACTIVE_PICKER,
        SKILL_PROMPT
    ),
    allow!(
        TRANSCRIPT,
        "write_output#1@2954ed28462f771b",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        UNIT
    ),
    allow!(
        TRANSCRIPT,
        "write_output#2@3e43deb9ab19a3bf",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        UNIT
    ),
    allow!(
        UI_DOCUMENT,
        "<module>#1@a550bb37792fb090",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_DOCUMENT,
        "<module>#2@46aa89441e8b06f3",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
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
        "write_frame#1@800a077f8a4bc2c0",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#1@9e176dd4991e94f2",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#2@26dbdba5809c8356",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#3@9e176dd4991e94f2",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#4@2a6861777f6f2eb8",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#5@c8b2ea0629c2b043",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#6@4f59ef5cdaf825f7",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#7@16487886cf6aa080",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "write_frame#8@257d49ad49992709",
        DirectWrite,
        Infrastructure,
        SPECIALIZED_STREAM,
        LIVE_OUTPUT
    ),
    allow!(
        UI_WRITER,
        "<module>#1@68508a6fdfc44ce9",
        UiRawWriter,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "<module>#1@8af2b2040e9e92e3",
        UiWriterInjection,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "<module>#2@154aef9fb34123dc",
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
        "write#1@62dfe1a34afb27b0",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "write#1@62dfe1a34afb27b0",
        DirectWrite,
        Infrastructure,
        UI_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        UPGRADE_HUMAN,
        "render_outcome#1@fdd8f1dd9ce705c8",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UPGRADE
    ),
    allow!(
        UPGRADE_STATUS,
        "render_status#1@79342920bb593fd0",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UPGRADE
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
];

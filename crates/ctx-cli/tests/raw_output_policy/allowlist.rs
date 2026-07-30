use super::{AllowEntry, OutputClass, Primitive};

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

const GATE: &str = "//crates/ctx-cli:raw_output_policy_tests";
const UNIT: &str = "//crates/ctx-cli:unit_tests";
const ANALYTICS: &str = "//crates/ctx-cli:analytics_policy_tests";
const INDEX: &str = "//crates/ctx-cli:index_tests";
const SEARCH_SQL: &str = "//crates/ctx-cli:search_show_locate_sql_tests";
const STATS: &str = "//crates/ctx-cli:stats_tests";
const STATUS: &str = "//crates/ctx-cli:status_store_cutover_tests";
const PUBLIC_HELP: &str = "//crates/ctx-cli:cli_public_help_docs_tests";
const MCP: &str = "//crates/ctx-cli:integrations_mcp_tests";
const MCP_SERVER: &str = "//crates/ctx-cli:mcp_tests";
const PRO: &str = "//crates/ctx-cli:pro_lifecycle_tests";
const SKILL: &str = "//crates/ctx-cli:skill_tests";
const SLASH: &str = "//crates/ctx-cli:slash_command_e2e_tests";
const UPGRADE: &str = "//crates/ctx-cli:upgrade_tests";

const CARGO_DIRECTIVE: &str = "Cargo build-script protocol directive";
const JSON_PROTOCOL: &str = "documented JSON or JSONL machine-output contract";
const TEXT_PROTOCOL: &str = "documented plain-text machine-output contract";
const DEBUG_DIAGNOSTIC: &str = "opt-in analytics debug diagnostic";
const TERMINAL_PROBE: &str = "terminal capability probe; emits no bytes";
const RAW_INFRASTRUCTURE: &str = "central raw-output infrastructure seam";
const UI_INFRASTRUCTURE: &str = "central Ui/Document rendering infrastructure seam";
const PLAIN_FALLBACK: &str = "plain-human fallback used before or outside Ui setup";
const SPECIALIZED_STREAM: &str = "specialized streaming renderer owns framing and writes";
const MACHINE_BODY: &str = "command emits a preformatted protocol body verbatim";

const BUILD: &str = "build.rs";
const ANALYTICS_SENDER: &str = "src/analytics/sender.rs";
const BLAME: &str = "src/commands/blame.rs";
const INDEX_COMMAND: &str = "src/commands/index.rs";
const SQL: &str = "src/commands/sql.rs";
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
const PRO_PENDING: &str = "src/pro/pending_materialization.rs";
const PRO_REFERRAL: &str = "src/pro/referral.rs";
const PRO_RENDER: &str = "src/pro/render.rs";
const PROGRESS: &str = "src/progress.rs";
const RELEASE_IDENTITY: &str = "src/release_build_identity.rs";
const SKILL_INSTALL: &str = "src/skill/install.rs";
const SKILL_SELECTION: &str = "src/skill/selection.rs";
const TRANSCRIPT: &str = "src/transcript.rs";
const UI_DOCUMENT: &str = "src/ui/document.rs";
const UI_MODULE: &str = "src/ui/mod.rs";
const UI_WRITER: &str = "src/ui/writer.rs";
const UPGRADE_HUMAN: &str = "src/upgrade/command/human.rs";
const UPGRADE_STATUS: &str = "src/upgrade/command/status.rs";
const WINDOWS_HELPER: &str = "src/upgrade/install/transaction/windows/helper.rs";

pub(super) const ALLOWLIST: &[AllowEntry] = &[
    allow!(
        BUILD,
        "main#1@3d618d0d6e1305c1",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#2@0947514f54f72ef8",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#3@148f1a006bdde4df",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#4@885cbe6e55e728c7",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#5@cfdf571c6929ef24",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#6@56760ef03ecfb333",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#7@0acadf523050c898",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#8@17e10d71b62c3756",
        PrintMacro,
        MachineProtocol,
        CARGO_DIRECTIVE,
        GATE
    ),
    allow!(
        BUILD,
        "main#9@90a37aff9068bc84",
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
        "run#1@862efb434b34b780",
        StdoutConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        UNIT
    ),
    allow!(
        BLAME,
        "run#1@862efb434b34b780",
        StderrConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        UNIT
    ),
    allow!(
        INDEX_COMMAND,
        "index_watch_output#1@834c396ba62e925f",
        UiRawWriter,
        Infrastructure,
        SPECIALIZED_STREAM,
        INDEX
    ),
    allow!(
        INDEX_COMMAND,
        "print_human#1@800a077f8a4bc2c0",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        INDEX
    ),
    allow!(
        SQL,
        "print_sql_truncation_notice#1@57049f8b574f87f2",
        PrintMacro,
        JustifiedPlainHuman,
        PLAIN_FALLBACK,
        SEARCH_SQL
    ),
    allow!(
        SQL,
        "print_sql_truncation_notice#2@f71aca3bc751a212",
        PrintMacro,
        JustifiedPlainHuman,
        PLAIN_FALLBACK,
        SEARCH_SQL
    ),
    allow!(
        SQL,
        "write_sql_stdout#1@6390ed6c2dce4746",
        OutputRawHelper,
        MachineProtocol,
        MACHINE_BODY,
        SEARCH_SQL
    ),
    allow!(
        STATS_COMMAND,
        "malformed_config_failure#1@d533fd75ee233f13",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATS
    ),
    allow!(
        STATUS_USAGE,
        "malformed_config_failure#1@448440e40ccc2c92",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS
    ),
    allow!(
        STATUS_USAGE,
        "removed_cloud_config_failure#1@1b71c97045dda36f",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS
    ),
    allow!(
        STATUS_USAGE,
        "usage_action_failure#1@62de96c8050fa336",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        STATUS
    ),
    allow!(
        DISPATCH,
        "render_generic_command_error#1@4fcd3f430927aff6",
        UiRawWriter,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        ANALYTICS
    ),
    allow!(
        DISPATCH,
        "run#1@e980ea9ca2d818d3",
        PrintMacro,
        JustifiedPlainHuman,
        PLAIN_FALLBACK,
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#1@1e006159573a8920",
        ClapParse,
        JustifiedPlainHuman,
        "clap owns help/version/parse-error output before Ui setup",
        PUBLIC_HELP
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
        "run_cli#1@34cd62977695262a",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#2@bec3fc86604eb591",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#3@bec3fc86604eb591",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UNIT
    ),
    allow!(
        DISPATCH,
        "run_cli#4@e980ea9ca2d818d3",
        PrintMacro,
        JustifiedPlainHuman,
        PLAIN_FALLBACK,
        UNIT
    ),
    allow!(
        DOCS,
        "list_docs#1@b952ea61fcba410a",
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
        "search_docs#1@b952ea61fcba410a",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        PUBLIC_HELP
    ),
    allow!(
        DOCS,
        "show_doc#1@e17478699cffefe1",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        PUBLIC_HELP
    ),
    allow!(
        MCP_OPERATION,
        "run_install#1@498281c376e92cd0",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        MCP
    ),
    allow!(
        MCP_OPERATION,
        "run_status#1@a565df5212a21b73",
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
        "<module>#4@80fd13410179b5e2",
        OutputRawHelper,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "<module>#5@270a7b6cfa1ef559",
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
        "stdout_writer#1@eb95a1f704d28b0f",
        StdoutConstructor,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#1@57e14a2db2574477",
        StdoutConstructor,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        OUTPUT,
        "write_stream#1@9d99ae52ba0872ab",
        StderrConstructor,
        Infrastructure,
        RAW_INFRASTRUCTURE,
        UNIT
    ),
    allow!(
        PRO_LIFECYCLE,
        "emit_uninstall_result#1@75eb9112501374a4",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_LIFECYCLE,
        "run_manage_with_opener#1@75eb9112501374a4",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PRO_LIFECYCLE,
        "run_setup#1@75eb9112501374a4",
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
        "defer_setup#1@9b41fc8d13cd0f7d",
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
        "write_cta#1@f53429f9d33162f3",
        DocumentRender,
        Infrastructure,
        UI_INFRASTRUCTURE,
        PRO
    ),
    allow!(
        PRO_RENDER,
        "print_blame_result#1@2048412be04f5f80",
        UiRawWriter,
        MachineProtocol,
        JSON_PROTOCOL,
        PRO
    ),
    allow!(
        PROGRESS,
        "emit_status_at#1@c56c9bd95dcc23cb",
        UiRawWriter,
        MachineProtocol,
        JSON_PROTOCOL,
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
        SKILL_INSTALL,
        "run_install#1@a1d0366871f3e9d3",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        SKILL
    ),
    allow!(
        SKILL_INSTALL,
        "run_status#1@b5985f1cf11f7d76",
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
        SKILL
    ),
    allow!(
        TRANSCRIPT,
        "write_output#1@3b5b225811599fd7",
        PrintMacro,
        MachineProtocol,
        MACHINE_BODY,
        UNIT
    ),
    allow!(
        TRANSCRIPT,
        "write_output#2@36628f9ebe42959b",
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
        "stream_width#1@6cc5fcce618aedb6",
        StdoutConstructor,
        CapabilityProbe,
        TERMINAL_PROBE,
        UNIT
    ),
    allow!(
        UI_WRITER,
        "stream_width#1@dadc3930bba5ddb0",
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
        UPGRADE_HUMAN,
        "render_outcome#1@b515bd1f7b8c527c",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UPGRADE
    ),
    allow!(
        UPGRADE_STATUS,
        "render_status#1@75eb9112501374a4",
        PrintMacro,
        MachineProtocol,
        JSON_PROTOCOL,
        UPGRADE
    ),
    allow!(
        WINDOWS_HELPER,
        "write_ready#1@7412c9b708be0f94",
        StdoutConstructor,
        MachineProtocol,
        "parent-process readiness protocol",
        UPGRADE
    ),
];

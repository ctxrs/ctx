"""Root-package wiring for the ctx-history-cli dependency boundary."""

load("@rules_shell//shell:sh_test.bzl", "sh_test")

_CARGO_WORKSPACE_BUILD_DATA = [
    "//crates/ctx-agent-application:BUILD.bazel",
    "//crates/ctx-agent-integrations:BUILD.bazel",
    "//crates/ctx-cli:BUILD.bazel",
    "//crates/ctx-cli-qualification:BUILD.bazel",
    "//crates/ctx-cli-presentation:BUILD.bazel",
    "//crates/ctx-client-observability:BUILD.bazel",
    "//crates/ctx-daemon-application:BUILD.bazel",
    "//crates/ctx-daemon-cli:BUILD.bazel",
    "//crates/ctx-daemon-runtime:BUILD.bazel",
    "//crates/ctx-daemon-service:BUILD.bazel",
    "//crates/ctx-history-capture-model:BUILD.bazel",
    "//crates/ctx-history-capture:BUILD.bazel",
    "//crates/ctx-history-provider-gemini:BUILD.bazel",
    "//crates/ctx-history-core:BUILD.bazel",
    "//crates/ctx-history-index-format:BUILD.bazel",
    "//crates/ctx-history-index-generation:BUILD.bazel",
    "//crates/ctx-history-index-query:BUILD.bazel",
    "//crates/ctx-history-index:BUILD.bazel",
    "//crates/ctx-history-ingest-application:BUILD.bazel",
    "//crates/ctx-history-jsonl:BUILD.bazel",
    "//crates/ctx-history-native-jsonl-parsers:BUILD.bazel",
    "//crates/ctx-deepseek-harness-qualification:BUILD.bazel",
    "//crates/ctx-history-read-application:BUILD.bazel",
    "//crates/ctx-history-refresh-execution:BUILD.bazel",
    "//crates/ctx-history-refresh:BUILD.bazel",
    "//crates/ctx-history-source-discovery:BUILD.bazel",
    "//crates/ctx-history-source-io:BUILD.bazel",
    "//crates/ctx-history-source-sqlite:BUILD.bazel",
    "//crates/ctx-managed-pair-engine:BUILD.bazel",
    "//crates/ctx-protocol:BUILD.bazel",
    "//crates/ctx-sdk:BUILD.bazel",
    "//crates/ctx-semantic-index:BUILD.bazel",
    "//crates/ctx-semantic-model:BUILD.bazel",
    "//crates/ctx-terminal:BUILD.bazel",
    "//crates/ctx-upgrade-engine:BUILD.bazel",
]

def history_cli_boundary(cargo_package_data, history_build_label):
    """Declares the complete static and evaluated history-CLI boundary gate."""
    native.exports_files([
        "BUILD.bazel",
        "Cargo.toml",
    ])

    native.filegroup(
        name = "history_cli_boundary_inputs",
        srcs = cargo_package_data + _CARGO_WORKSPACE_BUILD_DATA + [
            history_build_label,
            "BUILD.bazel",
            "Cargo.toml",
        ],
        visibility = ["//visibility:public"],
    )

    sh_test(
        name = "history_cli_boundary_check",
        srcs = ["//tools/bazel:check-history-cli-boundary.sh"],
        args = ["$(rootpath BUILD.bazel)"],
        data = [
            "BUILD.bazel",
            "Cargo.toml",
            "scripts/bazelw",
            ":history_cli_boundary_inputs",
            "//tools/bazel:check_history_cli_boundary.py",
        ],
        # This gate queries the complete live Bazel graph so composed labels and
        # loaded macros cannot evade its static inventory.
        tags = [
            "build-graph",
            "exclusive",
            "no-cache",
            "no-sandbox",
        ],
    )

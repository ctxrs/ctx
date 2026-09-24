"""Bazel-only final-binary contracts owned by ctx-daemon-cli."""

load("//tools/bazel:binary_contracts.bzl", "ctx_binary_contract_test")

_BASE_SUPPORT_DEPS = [
    "@crates//assert_cmd",
    "@crates//predicates",
    "@crates//:serde_json-1.0.150",
    "@crates//tempfile",
    "@crates//uuid",
]

_FIXTURE_SUPPORT_DEPS = [
    "@crates//libc",
    "@crates//rusqlite",
    "@crates//sha2",
    "@crates//windows-sys",
]

_UPGRADE_SUPPORT_DEPS = [
    "@crates//base64",
    "@crates//chrono",
    "@crates//flate2",
    "@crates//ring",
    "@crates//tar",
]

def daemon_cli_binary_contract(
        name,
        src,
        extra_deps = [],
        extra_env = {},
        extra_data = [],
        extra_srcs = [],
        fixtures = False,
        tags = [],
        upgrade = False):
    support_deps = _BASE_SUPPORT_DEPS + extra_deps
    support_srcs = [
        "tests/contracts/support.rs",
        "//crates/ctx-cli-contract-tests:contract_support_base",
    ]
    support_rustc_flags = []
    if fixtures:
        support_deps += _FIXTURE_SUPPORT_DEPS
        support_deps.append("//crates/ctx-history-index:lib")
        support_srcs.append("//crates/ctx-cli-contract-tests:contract_support_fixtures")
        support_rustc_flags.append("--cfg=ctx_cli_test_support_fixtures")
    if upgrade:
        support_deps += _UPGRADE_SUPPORT_DEPS
        support_deps.append("//crates/ctx-history-core:lib")
        support_srcs.append("//crates/ctx-cli-contract-tests:contract_support_upgrade")
        support_rustc_flags.append("--cfg=ctx_cli_test_support_upgrade")
    ctx_binary_contract_test(
        name = name,
        src = src,
        binary = "//crates/ctx-cli:ctx",
        cargo_manifest_dir = "crates/ctx-daemon-cli",
        support_deps = support_deps,
        support_srcs = support_srcs,
        support_rustc_flags = support_rustc_flags,
        extra_compile_data = [
            "//:ctx_bundled_skills",
            "//:ctx_embedded_docs",
            "//crates/ctx-cli:integration_test_compile_data",
        ],
        extra_data = [
            "//:public_test_fixtures",
            "//crates/ctx-cli:integration_test_data",
        ] + extra_data,
        extra_env = extra_env,
        extra_srcs = extra_srcs,
        rustc_env_files = ["//crates/ctx-daemon-cli:cargo_toml_env_vars"],
        tags = tags,
    )

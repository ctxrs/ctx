"""Bazel-only final-binary contracts owned by client observability."""

load("//tools/bazel:binary_contracts.bzl", "ctx_binary_contract_test")

_BASE_SUPPORT_DEPS = [
    "@crates//:assert_cmd-2.2.2",
    "@crates//:predicates-3.1.4",
    "@crates//:serde_json-1.0.150",
    "@crates//:tempfile-3.27.0",
    "@crates//:uuid-1.23.4",
]

_FIXTURE_SUPPORT_DEPS = [
    "@crates//:libc-0.2.186",
    "@crates//:rusqlite-0.40.2",
    "@crates//:sha2-0.10.9",
    "@crates//:windows-sys-0.61.2",
    "//crates/ctx-history-index:lib",
]

_UPGRADE_SUPPORT_DEPS = [
    "@crates//:base64-0.22.1",
    "@crates//:chrono-0.4.45",
    "@crates//:flate2-1.1.9",
    "@crates//:ring-0.17.14",
    "@crates//:tar-0.4.46",
    "//crates/ctx-history-core:lib",
]

def observability_binary_contract(
        name,
        src,
        binary = "//crates/ctx-cli:ctx",
        extra_deps = [],
        fixtures = False,
        tags = [],
        upgrade = False):
    support_deps = list(_BASE_SUPPORT_DEPS)
    support_srcs = [
        "tests/contracts/support.rs",
        "//crates/ctx-cli-contract-tests:contract_support_base",
    ]
    support_rustc_flags = []
    if fixtures:
        support_deps.extend(_FIXTURE_SUPPORT_DEPS)
        support_srcs.append("//crates/ctx-cli-contract-tests:contract_support_fixtures")
        support_rustc_flags.append("--cfg=ctx_cli_test_support_fixtures")
    if upgrade:
        support_deps.extend(_UPGRADE_SUPPORT_DEPS)
        if not fixtures:
            support_deps.append("@crates//:sha2-0.10.9")
        support_srcs.append("//crates/ctx-cli-contract-tests:contract_support_upgrade")
        support_rustc_flags.append("--cfg=ctx_cli_test_support_upgrade")

    ctx_binary_contract_test(
        name = name,
        src = src,
        binary = binary,
        cargo_manifest_dir = "crates/ctx-client-observability",
        support_deps = support_deps + extra_deps,
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
        ],
        rustc_env_files = ["//crates/ctx-cli:cargo_toml_env_vars"],
        tags = tags,
    )

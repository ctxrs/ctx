"""Bazel-only final-binary contracts owned by ctx-history-ingest-application."""

load("//tools/bazel:binary_contracts.bzl", "ctx_binary_contract_test")

_CONTRACT_SUPPORT_SRCS = [
    "tests/support/mod.rs",
    "tests/support/native_fixtures.rs",
    "//crates/ctx-cli-contract-tests:contract_support_base",
]

_CONTRACT_SUPPORT_DEPS = [
    "@crates//:assert_cmd",
    "@crates//:predicates",
    "@crates//:serde_json",
    "@crates//:tempfile",
    "@crates//:uuid",
    "@crates//:zstd",
]

def history_ingest_binary_contract(name, src, tags = []):
    ctx_binary_contract_test(
        name = name,
        src = src,
        binary = "//crates/ctx-cli:ctx",
        cargo_manifest_dir = "crates/ctx-history-ingest-application",
        support_deps = _CONTRACT_SUPPORT_DEPS,
        support_srcs = _CONTRACT_SUPPORT_SRCS,
        extra_compile_data = [
            "//:ctx_bundled_skills",
            "//:ctx_embedded_docs",
        ],
        extra_data = ["//:public_test_fixtures"],
        tags = tags,
    )

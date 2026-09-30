"""Bazel-only final-binary contracts owned by ctx-history-ingest-application."""

load("//tools/bazel:binary_contracts.bzl", "ctx_binary_contract_test")

_CONTRACT_SUPPORT_SRCS = [
    "tests/support/mod.rs",
    "tests/support/native_fixtures.rs",
    "//crates/ctx-cli-contract-tests:contract_support_base",
    "//crates/ctx-cli-contract-tests:contract_support_fixtures",
]

_CONTRACT_SUPPORT_DEPS = [
    "@crates//:assert_cmd",
    "@crates//:libc",
    "@crates//:predicates",
    "@crates//:rusqlite",
    "@crates//:serde_json-1.0.151",
    "@crates//:tempfile",
    "@crates//:uuid",
    "@crates//:windows-sys",
    "@crates//:zstd",
    "//crates/ctx-history-index:lib",
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

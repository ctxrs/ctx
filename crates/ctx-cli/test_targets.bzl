"""Native Bazel integration-test helper for the ctx CLI package."""

load("@crates//:defs.bzl", "aliases", "all_crate_deps", "crate_edition")
load("//tools/bazel:ctx_rust.bzl", "ctx_rust_test")

CTX_CLI_RUSTC_FLAGS = [
    "--check-cfg=cfg(ctx_pro_qualification)",
    "--check-cfg=cfg(ctx_pro_test_helper)",
    "--check-cfg=cfg(ctx_release_qualification)",
    "--check-cfg=cfg(ctx_semantic_fastembed)",
    "--check-cfg=cfg(test)",
] + select({
    "@rules_rust//rust/platform:aarch64-apple-darwin": [
        "--cfg=ctx_semantic_fastembed",
    ],
    "@rules_rust//rust/platform:aarch64-unknown-linux-gnu": [
        "--cfg=ctx_semantic_fastembed",
    ],
    "@rules_rust//rust/platform:x86_64-apple-darwin": [
        "--cfg=ctx_semantic_fastembed",
    ],
    "@rules_rust//rust/platform:x86_64-pc-windows-msvc": [
        "--cfg=ctx_semantic_fastembed",
    ],
    "@rules_rust//rust/platform:x86_64-unknown-freebsd": [
        "--cfg=ctx_semantic_fastembed",
    ],
    "@rules_rust//rust/platform:x86_64-unknown-linux-gnu": [
        "--cfg=ctx_semantic_fastembed",
    ],
    "@rules_rust//rust/platform:x86_64-unknown-nixos-gnu": [
        "--cfg=ctx_semantic_fastembed",
    ],
    "//conditions:default": [],
})

_CTX_CLI_DEPS = [
    "//crates/ctx-history-capture:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-index:lib",
    "//crates/ctx-history-relational:lib",
    "//crates/ctx-pro-host-protocol:lib",
]

def ctx_cli_test_data():
    return ["//:public_test_fixtures"] + native.glob([
        "testdata/**",
        "tests/fixtures/**",
    ])

def ctx_cli_integration_test(
        name,
        src,
        binary = ":ctx",
        crate_features = [],
        extra_env = {},
        extra_compile_data = [],
        extra_data = [],
        extra_deps = [],
        extra_srcs = [],
        tags = []):
    test_env = {
        "CARGO_BIN_EXE_ctx": "$(rootpath %s)" % binary,
    }
    test_env.update(extra_env)
    ctx_rust_test(
        name = name,
        srcs = [src] + extra_srcs + native.glob(["tests/support/**/*.rs"]),
        crate_name = name,
        crate_root = src,
        edition = crate_edition(),
        aliases = aliases(
            normal_dev = True,
            proc_macro_dev = True,
        ),
        compile_data = [
            "//:ctx_bundled_skills",
            "//:ctx_embedded_docs",
        ] + native.glob(["tests/fixtures/**"]) + extra_compile_data,
        crate_features = crate_features,
        data = ctx_cli_test_data() + [binary] + extra_data,
        deps = all_crate_deps(
            normal = True,
            normal_dev = True,
        ) + _CTX_CLI_DEPS + extra_deps,
        env = test_env,
        proc_macro_deps = all_crate_deps(
            proc_macro = True,
            proc_macro_dev = True,
        ),
        rustc_env = {"CARGO_MANIFEST_DIR": "crates/ctx-cli"},
        rustc_env_files = [":cargo_toml_env_vars"],
        rustc_flags = CTX_CLI_RUSTC_FLAGS,
        tags = tags,
    )

"""Bazel-only contracts that execute a final ctx binary without a Rust backedge."""

load("@crates//:defs.bzl", "aliases", "crate_deps", "crate_edition")
load("//tools/bazel:ctx_rust.bzl", "ctx_rust_test")

CTX_BINARY_CONTRACT_RUSTC_FLAGS = [
    "--check-cfg=cfg(ctx_release_qualification)",
    "--check-cfg=cfg(ctx_cli_test_support_fixtures)",
    "--check-cfg=cfg(ctx_cli_test_support_upgrade)",
    "--check-cfg=cfg(ctx_cli_bazel_test)",
    "--check-cfg=cfg(ctx_agent_application_contract_fixtures)",
    "--cfg=ctx_cli_bazel_test",
    "--check-cfg=cfg(test)",
]

_EXTRA_RUSTC_FLAGS = "@rules_rust//rust/settings:extra_rustc_flags"

def _optimized_fixture_transition_impl(settings, _attr):
    # Optimize the entire executable closure while retaining the fastbuild
    # harness's debug-only synchronization seams, including those in libraries.
    return {
        "//command_line_option:compilation_mode": "opt",
        _EXTRA_RUSTC_FLAGS: settings[_EXTRA_RUSTC_FLAGS] + ["-Cdebug-assertions=yes"],
    }

_optimized_fixture_transition = transition(
    implementation = _optimized_fixture_transition_impl,
    inputs = [_EXTRA_RUSTC_FLAGS],
    outputs = ["//command_line_option:compilation_mode", _EXTRA_RUSTC_FLAGS],
)

def _optimized_test_binary_impl(ctx):
    binary = ctx.attr.binary[0][DefaultInfo]
    return [DefaultInfo(
        files = depset([binary.files_to_run.executable]),
        runfiles = binary.default_runfiles,
    )]

_optimized_test_binary = rule(
    implementation = _optimized_test_binary_impl,
    attrs = {
        "binary": attr.label(
            cfg = _optimized_fixture_transition,
            executable = True,
            mandatory = True,
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    },
)

def ctx_optimized_test_binary(name, binary):
    """Exposes a complete optimized executable only as test runfiles data."""
    _optimized_test_binary(name = name, binary = binary, testonly = True)

def ctx_binary_contract_test(
        name,
        src,
        binary,
        cargo_manifest_dir,
        support_deps = [],
        support_srcs = [],
        support_rustc_flags = [],
        crate_features = [],
        extra_crates = [],
        extra_env = {},
        extra_compile_data = [],
        extra_data = [],
        extra_deps = [],
        extra_srcs = [],
        rustc_env_files = [],
        tags = []):
    """Compiles a standalone contract whose only product edge is executable data."""
    test_env = {
        "CARGO_BIN_EXE_ctx": "$(rootpath %s)" % binary,
    }
    test_env.update(extra_env)
    ctx_rust_test(
        name = name,
        srcs = [src] + support_srcs + extra_srcs,
        crate_name = name,
        crate_root = src,
        edition = crate_edition(),
        aliases = aliases(
            normal_dev = True,
            proc_macro_dev = True,
        ),
        compile_data = extra_compile_data,
        crate_features = crate_features,
        data = [binary] + extra_data,
        deps = support_deps + crate_deps(extra_crates) + extra_deps,
        env = test_env,
        proc_macro_deps = [],
        rustc_env = {"CARGO_MANIFEST_DIR": cargo_manifest_dir},
        rustc_env_files = rustc_env_files,
        rustc_flags = CTX_BINARY_CONTRACT_RUSTC_FLAGS + support_rustc_flags,
        tags = tags,
    )

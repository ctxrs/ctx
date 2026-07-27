"""Macros for explicit non-Rust repository gates."""

def non_rust_gate(name, mode, args = [], data = [], tags = []):
    native.sh_test(
        name = name,
        srcs = ["//:scripts/bazel-test.sh"],
        args = [mode] + args,
        data = data,
        tags = tags + ["non-rust-action"],
    )

def real_harness_gate(name, script_mode, binary, data):
    non_rust_gate(
        name = name,
        mode = script_mode,
        args = ["$(rootpath %s)" % binary],
        data = data,
        tags = ["external-harness", "manual"],
    )

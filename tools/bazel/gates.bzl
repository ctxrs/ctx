"""Macros for explicit non-Rust repository gates."""

_LOC_CHECK_EXTENSIONS = [
    "bash",
    "bzl",
    "c",
    "cc",
    "cjs",
    "cpp",
    "cs",
    "cxx",
    "go",
    "h",
    "hh",
    "hpp",
    "hxx",
    "java",
    "js",
    "jsx",
    "kt",
    "kts",
    "mjs",
    "ps1",
    "psm1",
    "py",
    "rs",
    "sh",
    "swift",
    "ts",
    "tsx",
]

_LOC_CHECK_EXCLUDES = [
    "**/data/**",
    "**/docs/**",
    "**/fixture/**",
    "**/fixtures/**",
    "**/gen/**",
    "**/generated/**",
    "data/**",
    "docs/**",
    "fixture/**",
    "fixtures/**",
    "gen/**",
    "generated/**",
]

def loc_check_inputs(name):
    """Declares every file in this package that check-loc.sh can classify."""
    patterns = [
        "MODULE.bazel",
        "WORKSPACE",
        "WORKSPACE.bazel",
    ]
    patterns += ["*.%s" % extension for extension in _LOC_CHECK_EXTENSIONS]
    patterns += ["**/*.%s" % extension for extension in _LOC_CHECK_EXTENSIONS]
    native.filegroup(
        name = name,
        srcs = ["BUILD.bazel"] + native.glob(
            patterns,
            exclude = _LOC_CHECK_EXCLUDES,
        ),
        visibility = ["//visibility:public"],
    )

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

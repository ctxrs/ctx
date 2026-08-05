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
    "bazel-*/**",
    "external/**",
    "target/**",
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

def _loc_check_manifest_impl(ctx):
    paths = sorted({source.short_path: True for source in ctx.files.srcs}.keys())
    ctx.actions.write(
        output = ctx.outputs.out,
        content = "\n".join(paths) + "\n",
    )
    return [DefaultInfo(files = depset([ctx.outputs.out]))]

_loc_check_manifest = rule(
    implementation = _loc_check_manifest_impl,
    attrs = {"srcs": attr.label_list(allow_files = True)},
    outputs = {"out": "%{name}.txt"},
)

def loc_check_manifest(name, srcs):
    """Writes the exact declared LOC source inventory for sandboxed checks."""
    _loc_check_manifest(name = name, srcs = srcs)

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

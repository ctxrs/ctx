"""Common Rust target policy for the public ctx Bazel graph."""

load("@rules_rust//rust:defs.bzl", _rust_binary = "rust_binary", _rust_test = "rust_test")

_MOLD_DATA = select({
    "//:dev_linux": ["//tools/bazel:mold_toolchain"],
    "//conditions:default": [],
})

_MOLD_FLAGS = select({
    "//:dev_linux": [
        "-Clink-arg=-fuse-ld=mold",
        "-Clink-arg=-B$(execpath //tools/bazel:mold_toolchain)",
    ],
    "//conditions:default": [],
})

# Own the final Mach-O load-command minimum in the target link action as well
# as the release action environment. The explicit linker argument is scoped to
# Apple targets and prevents an ambient host SDK default from becoming ABI.
_MACOS_LINK_FLAGS = select({
    "@platforms//os:macos": ["-Clink-arg=-mmacosx-version-min=13.0"],
    "//conditions:default": [],
})

def ctx_rust_binary(name, data = [], rustc_flags = [], tags = [], **kwargs):
    """Creates a Rust binary with the configured, tracked development linker."""
    _rust_binary(
        name = name,
        data = data + _MOLD_DATA,
        rustc_flags = rustc_flags + _MOLD_FLAGS + _MACOS_LINK_FLAGS,
        tags = tags,
        **kwargs
    )

def ctx_rust_test(name, data = [], rustc_flags = [], tags = [], **kwargs):
    """Creates a Rust test with the configured, tracked development linker."""
    _rust_test(
        name = name,
        data = data + _MOLD_DATA,
        rustc_flags = rustc_flags + _MOLD_FLAGS + _MACOS_LINK_FLAGS,
        tags = tags,
        **kwargs
    )

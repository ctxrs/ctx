"""Pinned LLVM-MinGW repository used by the Windows GNU release route."""

def _llvm_mingw_repository_impl(ctx):
    ctx.download_and_extract(
        integrity = ctx.attr.integrity,
        sha256 = ctx.attr.sha256,
        stripPrefix = ctx.attr.strip_prefix,
        url = ctx.attr.urls,
    )

    # Rust's windows-gnu standard library passes -nodefaultlibs and requests
    # GCC-compatible runtime names explicitly. Preserve that link order while
    # mapping the names to LLVM-MinGW's equivalent static runtimes. Bazel uses
    # native symlinks where available and copies file targets on Windows hosts
    # without symlink support.
    ctx.symlink(
        "x86_64-w64-mingw32/lib/libunwind.a",
        "x86_64-w64-mingw32/lib/libgcc_eh.a",
    )
    ctx.symlink(
        "lib/clang/22/lib/windows/libclang_rt.builtins-x86_64.a",
        "x86_64-w64-mingw32/lib/libgcc.a",
    )
    ctx.file(
        "BUILD.bazel",
        """
load("@ctx_search//tools/bazel/mingw:cc_toolchain_config.bzl", "llvm_mingw_cc_toolchain_config")
load("@rules_cc//cc/toolchains:cc_toolchain.bzl", "cc_toolchain")

package(default_visibility = ["//visibility:public"])

filegroup(
    name = "toolchain_files",
    srcs = glob(["**"], exclude = ["BUILD.bazel"]),
)

llvm_mingw_cc_toolchain_config(name = "config")

cc_toolchain(
    name = "cc_toolchain",
    all_files = ":toolchain_files",
    ar_files = ":toolchain_files",
    compiler_files = ":toolchain_files",
    dwp_files = ":toolchain_files",
    linker_files = ":toolchain_files",
    objcopy_files = ":toolchain_files",
    strip_files = ":toolchain_files",
    supports_param_files = 1,
    toolchain_config = ":config",
)

toolchain(
    name = "windows_x64_gnu_toolchain",
    exec_compatible_with = [
        "@platforms//cpu:x86_64",
        "@platforms//os:windows",
    ],
    target_compatible_with = [
        "@ctx_search//tools/bazel/platforms:windows_gnu",
        "@platforms//cpu:x86_64",
        "@platforms//os:windows",
    ],
    toolchain = ":cc_toolchain",
    toolchain_type = "@bazel_tools//tools/cpp:toolchain_type",
)
""",
    )

llvm_mingw_repository = repository_rule(
    implementation = _llvm_mingw_repository_impl,
    attrs = {
        "integrity": attr.string(),
        "sha256": attr.string(mandatory = True),
        "strip_prefix": attr.string(mandatory = True),
        "urls": attr.string_list(mandatory = True),
    },
)

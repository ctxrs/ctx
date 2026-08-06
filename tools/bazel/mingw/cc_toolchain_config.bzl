"""Hermetic LLVM-MinGW C/C++ toolchain configuration."""

load("@rules_cc//cc:cc_toolchain_config_lib.bzl", "tool_path")
load("@rules_cc//cc/common:cc_common.bzl", "cc_common")

def _impl(ctx):
    return cc_common.create_cc_toolchain_config_info(
        ctx = ctx,
        abi_libc_version = "msvcrt",
        abi_version = "x86_64",
        compiler = "clang",
        host_system_name = "windows",
        target_cpu = "x86_64",
        target_libc = "mingw",
        target_system_name = "x86_64-w64-mingw32",
        tool_paths = [
            tool_path(name = "ar", path = "bin/llvm-ar.exe"),
            tool_path(name = "cpp", path = "bin/x86_64-w64-mingw32-clang.exe"),
            tool_path(name = "gcc", path = "bin/x86_64-w64-mingw32-clang.exe"),
            tool_path(name = "gcov", path = "bin/llvm-cov.exe"),
            tool_path(name = "ld", path = "bin/x86_64-w64-mingw32-clang.exe"),
            tool_path(name = "nm", path = "bin/llvm-nm.exe"),
            tool_path(name = "objcopy", path = "bin/llvm-objcopy.exe"),
            tool_path(name = "objdump", path = "bin/llvm-objdump.exe"),
            tool_path(name = "strip", path = "bin/llvm-strip.exe"),
        ],
        toolchain_identifier = "ctx-llvm-mingw-20260616-msvcrt-x86_64",
    )

llvm_mingw_cc_toolchain_config = rule(
    implementation = _impl,
)

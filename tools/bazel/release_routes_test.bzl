"""Focused analysis tests for public release route transitions."""

load(
    ":release_routes.bzl",
    "ReleaseRouteInfo",
    "public_cli_release_route",
)

def _platform_probe_impl(ctx):
    for value in ctx.attr.constraint_values:
        if not ctx.target_platform_has_constraint(value[platform_common.ConstraintValueInfo]):
            fail("{} did not select required target constraint {}".format(
                ctx.label,
                value.label,
            ))
    actual_windows_gnu = ctx.attr.windows_gnu_value == "true"
    if actual_windows_gnu != ctx.attr.expect_windows_gnu:
        fail("{} selected windows_gnu_release={}, expected {}".format(
            ctx.label,
            actual_windows_gnu,
            ctx.attr.expect_windows_gnu,
        ))
    expected_abi = "gnu" if ctx.attr.expect_windows_gnu else "none"
    if ctx.attr.windows_abi_value != expected_abi:
        fail("{} selected Windows ABI {}, expected {}".format(
            ctx.label,
            ctx.attr.windows_abi_value,
            expected_abi,
        ))
    output = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(output, "#!/usr/bin/env sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = output)]

_platform_probe = rule(
    implementation = _platform_probe_impl,
    executable = True,
    attrs = {
        "constraint_values": attr.label_list(
            mandatory = True,
            providers = [platform_common.ConstraintValueInfo],
        ),
        "expect_windows_gnu": attr.bool(default = False),
        "windows_gnu_value": attr.string(mandatory = True),
        "windows_abi_value": attr.string(mandatory = True),
    },
)

def _plain_executable_impl(ctx):
    output = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(output, "#!/usr/bin/env sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = output)]

_plain_executable = rule(
    implementation = _plain_executable_impl,
    executable = True,
)

def _runfiles_probe_packager_impl(ctx):
    output = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(
        output,
        """#!/usr/bin/env bash
set -euo pipefail
: "${RUNFILES_DIR:?release route did not forward its derived runfiles root}"
workspace="${TEST_WORKSPACE:-_main}"
test -e "${RUNFILES_DIR}/${workspace}/ctx_release_routes/linux-x64/artifact"
test -e "${RUNFILES_DIR}/${workspace}/ctx_release_routes/linux-x64/rustc"
""",
        is_executable = True,
    )
    return [DefaultInfo(executable = output)]

_runfiles_probe_packager = rule(
    implementation = _runfiles_probe_packager_impl,
    executable = True,
)

def _route_analysis_test_impl(ctx):
    info = ctx.attr.route[ReleaseRouteInfo]
    if info.target_id != ctx.attr.target_id:
        fail("route target id is {}, expected {}".format(
            info.target_id,
            ctx.attr.target_id,
        ))
    if info.target_triple != ctx.attr.target_triple:
        fail("route target triple is {}, expected {}".format(
            info.target_triple,
            ctx.attr.target_triple,
        ))
    if info.available != ctx.attr.available:
        fail("route availability is {}, expected {}".format(
            info.available,
            ctx.attr.available,
        ))
    output = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(output, "#!/usr/bin/env sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = output)]

_route_analysis_test = rule(
    implementation = _route_analysis_test_impl,
    test = True,
    attrs = {
        "available": attr.bool(default = True),
        "route": attr.label(mandatory = True, providers = [ReleaseRouteInfo]),
        "target_id": attr.string(mandatory = True),
        "target_triple": attr.string(mandatory = True),
    },
)

def release_route_analysis_test_suite(name):
    """Instantiates probes that force each owned route transition to analyze."""
    _plain_executable(
        name = "_release_route_test_packager",
        tags = ["manual", "release-gate"],
    )
    _plain_executable(
        name = "_release_route_test_inventory",
        tags = ["manual", "release-gate"],
    )
    _plain_executable(
        name = "_release_route_test_license_materials",
        tags = ["manual", "release-gate"],
    )
    route_specs = {
        "linux_x64": struct(
            constraints = ["@platforms//cpu:x86_64", "@platforms//os:linux"],
            id = "linux-x64",
            triple = "x86_64-unknown-linux-gnu",
            windows_gnu = False,
        ),
        "linux_arm64": struct(
            constraints = ["@platforms//cpu:aarch64", "@platforms//os:linux"],
            id = "linux-arm64",
            triple = "aarch64-unknown-linux-gnu",
            windows_gnu = False,
        ),
        "macos_arm64": struct(
            constraints = ["@platforms//cpu:aarch64", "@platforms//os:osx"],
            id = "macos-arm64",
            triple = "aarch64-apple-darwin",
            windows_gnu = False,
        ),
        "macos_x64": struct(
            constraints = ["@platforms//cpu:x86_64", "@platforms//os:osx"],
            id = "macos-x64",
            triple = "x86_64-apple-darwin",
            windows_gnu = False,
        ),
        "windows_x64": struct(
            constraints = [
                "//tools/bazel/platforms:windows_gnu",
                "@platforms//cpu:x86_64",
                "@platforms//os:windows",
            ],
            id = "windows-x64",
            triple = "x86_64-pc-windows-gnu",
            windows_gnu = True,
        ),
        "freebsd_x64": struct(
            constraints = ["@platforms//cpu:x86_64", "@platforms//os:freebsd"],
            id = "freebsd-x64",
            triple = "x86_64-unknown-freebsd",
            windows_gnu = False,
        ),
    }
    tests = []
    for suffix, spec in route_specs.items():
        probe_name = "_release_route_probe_{}".format(suffix)
        route_name = "_release_route_{}".format(suffix)
        test_name = "_release_route_{}_analysis_test".format(suffix)
        _platform_probe(
            name = probe_name,
            constraint_values = spec.constraints,
            expect_windows_gnu = spec.windows_gnu,
            tags = ["manual", "release-gate"],
            windows_gnu_value = select({
                "//tools/bazel:windows_gnu_toolchain": "true",
                "//conditions:default": "false",
            }),
            windows_abi_value = select({
                "//tools/bazel/platforms:x86_64-pc-windows-gnu": "gnu",
                "//tools/bazel/platforms:x86_64-pc-windows-msvc": "msvc",
                "//conditions:default": "none",
            }),
        )
        public_cli_release_route(
            name = route_name,
            artifact = probe_name,
            license_materials = ":_release_route_test_license_materials",
            packager = ":_release_route_test_packager",
            rustc = probe_name,
            sbom_inventory = ":_release_route_test_inventory",
            target_id = spec.id,
        )
        _route_analysis_test(
            name = test_name,
            route = route_name,
            tags = ["release-gate"],
            target_id = spec.id,
            target_triple = spec.triple,
        )
        tests.append(test_name)

    _runfiles_probe_packager(
        name = "_release_route_runfiles_probe_packager",
        tags = ["manual", "release-gate"],
    )
    public_cli_release_route(
        name = "_release_route_runfiles_probe",
        artifact = ":_release_route_probe_linux_x64",
        license_materials = ":_release_route_test_license_materials",
        packager = ":_release_route_runfiles_probe_packager",
        rustc = ":_release_route_probe_linux_x64",
        sbom_inventory = ":_release_route_test_inventory",
        target_id = "linux-x64",
    )
    native.sh_test(
        name = "_release_route_runfiles_runtime_test",
        srcs = ["release_route_runfiles_test.sh"],
        data = [":_release_route_runfiles_probe"],
        tags = ["release-gate"],
    )
    tests.append("_release_route_runfiles_runtime_test")

    native.test_suite(
        name = name,
        tests = tests,
    )

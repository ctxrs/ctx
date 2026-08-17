#!/usr/bin/env python3
"""Validate provision-free Linux SDK CI and the shared SDK runner."""

from __future__ import annotations

import re
import sys


class SDKRouteError(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SDKRouteError(message)


LINUX_SDK_INVOCATION = (
    "bash scripts/check-sdks.sh "
    "--groups=contracts,typescript,python,go,jvm,dotnet "
    "--required-groups=contracts,typescript,python,go,jvm,dotnet"
)
REQUIRED_COMMANDS = (
    "bash",
    "cc",
    "c++",
    "curl",
    "dbus-daemon",
    "dotnet",
    "git",
    "java",
    "javac",
    "jq",
    "make",
    "node",
    "npm",
    "openssl",
    "pkg-config",
    "python3",
    "rg",
    "ruby",
    "unzip",
    "zip",
)
PROVISIONING_MARKERS = (
    "apt-get",
    "dpkg-query",
    "install_ubuntu_tools",
    "run_apt_get",
    "DEBIAN_FRONTEND",
)


def top_level_commands(source: str) -> list[str]:
    parts = source.rsplit("\n}\n", 1)
    require(len(parts) == 2, "Linux CI must end function definitions before execution")
    commands = []
    for line in parts[1].splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        require(line == line.lstrip(), "Linux CI execution commands must be top-level")
        commands.append(line)
    return commands


def declared_required_commands(source: str) -> tuple[str, ...]:
    match = re.search(
        r"local required_commands=\(\n(?P<body>(?:    [^\n]+\n)+)  \)", source
    )
    require(match is not None, "Linux CI must declare its worker command capabilities")
    return tuple(line.strip() for line in match.group("body").splitlines())


def validate_linux_route(source: str) -> None:
    for marker in PROVISIONING_MARKERS:
        require(marker not in source, f"Linux CI must not provision workers through {marker}")
    for marker, message in (
        (
            'export CTX_BAZEL_TEST_TMPDIR="${CTX_PUBLIC_CI_TEST_TMPDIR:-${tool_root}/bazel-test-tmp}"',
            "release Bazel tests must inherit the task-local temporary bind",
        ),
        (
            "if libc.unshare(clone_fs) != 0:",
            "release validation must preflight exact CLONE_FS authority",
        ),
        (
            "require_preinstalled_tools() {",
            "Linux CI must fail closed on an unprepared worker",
        ),
        (
            "/etc/ssl/certs/ca-certificates.crt",
            "Linux CI must require a preinstalled CA certificate bundle",
        ),
        (
            "python3 -c 'import build, pip, venv'",
            "Linux CI must require its Python build modules",
        ),
        (
            "Buildkite worker image is unprepared; missing required commands:",
            "missing worker tools must produce a clear diagnostic",
        ),
    ):
        require(source.count(marker) == 1, message)
    require(
        declared_required_commands(source) == REQUIRED_COMMANDS,
        "Linux CI worker capability inventory changed",
    )
    require(
        top_level_commands(source)
        == [
            "init_buildkite_job_tool_env",
            "preflight_release_test_authority",
            "require_preinstalled_tools",
            "configure_bazelisk",
            "print_tool_versions",
            LINUX_SDK_INVOCATION,
            'bash scripts/check.sh "${check_args[@]}"',
        ],
        "Linux CI must retain its exact fail-closed execution sequence",
    )


def validate_sdk_runner(source: str) -> None:
    required_commands = (
        'all_groups="contracts,typescript,python,go,jvm,swift,dotnet"',
        "check_version typescript Node.js 20.0",
        'run_in_dir "${typescript_root}" npm test --prefix sdks/typescript',
        "run python3 -m unittest discover -s sdks/python/tests",
        "//sdks/go:go_sdk_tests",
        "check_version jvm Java 11.0",
        "jvm_test='sdks/jvm/scripts/test'",
        'run "${jvm_test}"',
        'run swift test --package-path sdks/swift --scratch-path "$tmp_dir/swift-build"',
        "check_swift_version 5.9",
        "check_version dotnet .NET 8.0",
        'run dotnet build "${dotnet_tests}" --configuration Release --nologo',
        'run dotnet run --project "${dotnet_tests}" --configuration Release --no-build',
    )
    for required_command in required_commands:
        require(
            source.count(required_command) == 1,
            f"SDK runner must retain exact command: {required_command}",
        )


def validate(public_ci: str, sdk_runner: str) -> None:
    validate_linux_route(public_ci)
    validate_sdk_runner(sdk_runner)


def expect_rejection(
    name: str,
    public_ci: str,
    sdk_runner: str,
) -> None:
    try:
        validate(public_ci, sdk_runner)
    except SDKRouteError as error:
        print(f"Buildkite SDK self-test ok: {name} rejected ({error})")
        return
    raise SystemExit(f"Buildkite SDK self-test failed: {name} was accepted")


def replace_once(source: str, old: str, new: str) -> str:
    require(source.count(old) == 1, f"self-test fixture must contain {old!r} once")
    return source.replace(old, new, 1)


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: check-sdk-ci-pipeline.py PUBLIC_CI SDK_RUNNER")
    public_ci, sdk_runner = [
        open(path, encoding="utf-8").read() for path in sys.argv[1:]
    ]
    validate(public_ci, sdk_runner)

    expect_rejection(
        "worker provisioning restored",
        replace_once(
            public_ci,
            "require_preinstalled_tools\n",
            "apt-get update\nrequire_preinstalled_tools\n",
        ),
        sdk_runner,
    )
    expect_rejection(
        "required worker command removed",
        replace_once(public_ci, "    dotnet\n", ""),
        sdk_runner,
    )
    for name, replacement in (
        ("Linux SDK failures ignored", f"{LINUX_SDK_INVOCATION} || true"),
        ("Linux SDK bypassed by early exit", f"exit 0\n{LINUX_SDK_INVOCATION}"),
        (
            "Linux SDK groups made optional",
            LINUX_SDK_INVOCATION.replace(
                " --required-groups=contracts,typescript,python,go,jvm,dotnet", ""
            ),
        ),
    ):
        expect_rejection(
            name,
            replace_once(public_ci, LINUX_SDK_INVOCATION, replacement),
            sdk_runner,
        )
    for name, exact_command in (
        ("JVM canonical script replaced", "jvm_test='sdks/jvm/scripts/test'"),
        ("JVM test removed", 'run "${jvm_test}"'),
        (
            "Swift test removed",
            'run swift test --package-path sdks/swift --scratch-path "$tmp_dir/swift-build"',
        ),
        (
            ".NET build removed",
            'run dotnet build "${dotnet_tests}" --configuration Release --nologo',
        ),
    ):
        expect_rejection(
            name,
            public_ci,
            replace_once(sdk_runner, exact_command, "true"),
        )
    print("Buildkite provision-free SDK route check ok")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Validate that every SDK suite has a required Buildkite execution route."""

import re
import sys


class SDKRouteError(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SDKRouteError(message)


def scalar(block: str, name: str, indent: int = 4, required: bool = True):
    prefix = " " * indent
    matches = re.findall(
        rf"^{re.escape(prefix + name)}:[ \t]*(.+?)[ \t]*$",
        block,
        flags=re.MULTILINE,
    )
    if not matches and not required:
        return None
    require(len(matches) == 1, f"step must define exactly one {name}")
    value = matches[0]
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        value = value[1:-1]
    return value


def step_key(block: str):
    return scalar(block, "key", required=False)


def command(block: str) -> str:
    match = re.search(
        r"^    command: \|\n((?:^      .*(?:\n|$))*)",
        block,
        flags=re.MULTILINE,
    )
    require(match is not None, "required SDK step must define a block command")
    return "\n".join(line[6:] for line in match.group(1).splitlines()).strip()


def split_steps(source: str):
    lines = source.splitlines(keepends=True)
    try:
        steps_line = next(
            index for index, line in enumerate(lines) if line.rstrip() == "steps:"
        )
    except StopIteration as error:
        raise SDKRouteError("pipeline must define top-level steps") from error
    starts = [
        index
        for index in range(steps_line + 1, len(lines))
        if lines[index].startswith("  - ")
    ]
    require(bool(starts), "pipeline must define top-level steps")
    starts.append(len(lines))
    return ["".join(lines[start:end]) for start, end in zip(starts, starts[1:])]


REQUIRED_CONDITION = (
    'build.source != "schedule" || '
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1"'
)

SDK_SPECS = {
    "sdk-swift-required": {
        "command": (
            "javac -version\n"
            "swift --version\n"
            "bash scripts/check-sdks.sh --groups=jvm,swift --required-groups=jvm,swift"
        ),
        "queue": "mac-shared",
        "os": "darwin",
        "arch": "arm64",
        "concurrency_group": "ctx/sdk-swift-required/mac-shared",
    },
    "sdk-windows-required": {
        "command": (
            "export DOTNET_CLI_TELEMETRY_OPTOUT=1\n"
            "export DOTNET_NOLOGO=1\n"
            "node --version\n"
            "npm --version\n"
            "python3 --version\n"
            "go version\n"
            "dotnet --info\n"
            "bash scripts/check-sdks.sh "
            "--groups=typescript,python,go,dotnet "
            "--required-groups=typescript,python,go,dotnet\n"
            "bash scripts/bazelw test //crates/ctx-sdk:unit_tests --config=ci"
        ),
        "queue": "windows-x64",
        "os": "windows",
        "arch": "x86_64",
        "concurrency_group": "ctx/sdk-windows-required/windows-x64",
    },
}


def validate_sdk_steps(blocks) -> None:
    release_waits = [
        index
        for index, block in enumerate(blocks)
        if block.startswith("  - wait:")
        and scalar(block, "if", required=False)
        == 'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1"'
    ]
    require(len(release_waits) == 1, "required SDK routes need one release wait")
    release_wait = release_waits[0]
    for key, spec in SDK_SPECS.items():
        indexes = [index for index, block in enumerate(blocks) if step_key(block) == key]
        require(len(indexes) == 1, f"pipeline must define exactly one {key} route")
        index = indexes[0]
        block = blocks[index]
        require(index < release_wait, f"{key} must complete before release promotion")
        require(
            scalar(block, "if") == REQUIRED_CONDITION,
            f"{key} must run on every PR, merge, and release build",
        )
        require(
            command(block) == spec["command"],
            f"{key} must invoke its exact fail-closed SDK command",
        )
        for field in ("soft_fail", "skip", "allow_dependency_failure", "depends_on"):
            require(
                scalar(block, field, required=False) is None,
                f"{key} must not be optional through {field}",
            )
        for field in ("queue", "os", "arch"):
            require(
                scalar(block, field, indent=6) == spec[field],
                f"{key} must require {field}={spec[field]}",
            )
        require(
            scalar(block, "concurrency") == "1"
            and scalar(block, "concurrency_group") == spec["concurrency_group"],
            f"{key} must retain its required host concurrency group",
        )
        require(
            scalar(block, "timeout_in_minutes") == "30",
            f"{key} must retain its bounded timeout",
        )


def validate_linux_route(source: str) -> None:
    invocation = (
        "bash scripts/check-sdks.sh "
        "--groups=contracts,typescript,python,go,jvm,dotnet "
        "--required-groups=contracts,typescript,python,go,jvm,dotnet"
    )
    require(
        source.count(invocation) == 1,
        "Linux CI must require contracts, TypeScript, Python, Go, JVM, and .NET once",
    )


def validate_sdk_runner(source: str) -> None:
    required_commands = (
        'all_groups="contracts,typescript,python,go,jvm,swift,dotnet"',
        "check_version typescript Node.js 20.0",
        'run_in_dir "${typescript_root}" npm test --prefix sdks/typescript',
        "run python3 -m unittest discover -s sdks/python/tests",
        "//sdks/go:go_sdk_tests",
        "check_version jvm Java 11.0",
        'run swift test --package-path sdks/swift --scratch-path "$tmp_dir/swift-build"',
        "check_version swift Swift 5.9",
        "check_version dotnet .NET 8.0",
        'run dotnet build "${dotnet_tests}" --configuration Release --nologo',
        'run dotnet run --project "${dotnet_tests}" --configuration Release --no-build',
    )
    for required_command in required_commands:
        require(
            source.count(required_command) == 1,
            f"SDK runner must retain exact command: {required_command}",
        )


def validate(pipeline: str, public_ci: str, sdk_runner: str) -> None:
    validate_sdk_steps(split_steps(pipeline))
    validate_linux_route(public_ci)
    validate_sdk_runner(sdk_runner)


def mutate_step(blocks, key: str, old: str, new: str):
    mutated = []
    changed = 0
    for block in blocks:
        if step_key(block) == key:
            require(old in block, f"self-test fixture missing {old!r}")
            block = block.replace(old, new, 1)
            changed += 1
        mutated.append(block)
    require(changed == 1, f"self-test must mutate exactly one {key} step")
    return mutated


def join_steps(blocks) -> str:
    return "steps:\n" + "".join(blocks)


def expect_rejection(name: str, pipeline: str, public_ci: str, sdk_runner: str) -> None:
    try:
        validate(pipeline, public_ci, sdk_runner)
    except SDKRouteError as error:
        print(f"Buildkite SDK self-test ok: {name} rejected ({error})")
        return
    raise SystemExit(f"Buildkite SDK self-test failed: {name} was accepted")


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit(
            "usage: check-sdk-ci-pipeline.py PIPELINE PUBLIC_CI SDK_RUNNER"
        )
    pipeline, public_ci, sdk_runner = [
        open(path, encoding="utf-8").read() for path in sys.argv[1:]
    ]
    validate(pipeline, public_ci, sdk_runner)
    blocks = split_steps(pipeline)
    for key in SDK_SPECS:
        expect_rejection(
            f"missing {key}",
            join_steps(block for block in blocks if step_key(block) != key),
            public_ci,
            sdk_runner,
        )
    mutations = (
        ("optional Swift", "sdk-swift-required", "    timeout_in_minutes: 30\n", "    soft_fail: true\n    timeout_in_minutes: 30\n"),
        ("skipped Windows", "sdk-windows-required", "    timeout_in_minutes: 30\n", "    skip: true\n    timeout_in_minutes: 30\n"),
        ("optional macOS command", "sdk-swift-required", " --required-groups=jvm,swift", ""),
        ("optional Windows command", "sdk-windows-required", " --required-groups=typescript,python,go,dotnet", ""),
        ("PR-excluding Windows condition", "sdk-windows-required", 'build.source != "schedule" || ', ""),
        ("Windows Rust SDK removed", "sdk-windows-required", "bash scripts/bazelw test //crates/ctx-sdk:unit_tests --config=ci", "true"),
    )
    for name, key, old, new in mutations:
        expect_rejection(
            name,
            join_steps(mutate_step(blocks, key, old, new)),
            public_ci,
            sdk_runner,
        )
    expect_rejection(
        "TypeScript removed from Linux requirements",
        pipeline,
        public_ci.replace(
            "--required-groups=contracts,typescript,python,go,jvm,dotnet",
            "--required-groups=contracts,python,go,jvm,dotnet",
            1,
        ),
        sdk_runner,
    )
    expect_rejection(
        "Linux SDK groups made optional",
        pipeline,
        public_ci.replace(
            " --required-groups=contracts,typescript,python,go,jvm,dotnet", "", 1
        ),
        sdk_runner,
    )
    for label, exact_command in (
        ("Swift test removed", 'run swift test --package-path sdks/swift --scratch-path "$tmp_dir/swift-build"'),
        (".NET build removed", 'run dotnet build "${dotnet_tests}" --configuration Release --nologo'),
    ):
        expect_rejection(
            label,
            pipeline,
            public_ci,
            sdk_runner.replace(exact_command, "true", 1),
        )
    print("Buildkite required SDK route check ok")


if __name__ == "__main__":
    main()

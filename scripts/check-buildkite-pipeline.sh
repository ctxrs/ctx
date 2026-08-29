#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
for required in \
  "${pipeline}" \
  scripts/buildkite-public-ci.sh \
  scripts/buildkite/download-linux-factory-artifacts.sh \
  scripts/release/build-public-candidate-on-linux.sh \
  scripts/validate-public-cli-factory-artifact.sh \
  scripts/stage-github-release-assets.sh \
  scripts/qualify-macos-release-pair.sh \
  scripts/assemble-github-release-assets.sh \
  scripts/stage-semantic-release-handoff.sh \
  scripts/build-onnxruntime-sidecar.sh \
  scripts/check-sdks.sh; do
  [[ -f "${required}" ]] || {
    printf 'Buildkite release input missing: %s\n' "${required}" >&2
    exit 1
  }
done

python3 - "${pipeline}" <<'PY'
from __future__ import annotations

import copy
import json
import re
import subprocess
import sys


def fail(message: str) -> None:
    raise SystemExit(f"Buildkite release pipeline: {message}")


try:
    encoded = subprocess.check_output(
        [
            "ruby",
            "-rjson",
            "-ryaml",
            "-e",
            "print JSON.generate(YAML.load_file(ARGV.fetch(0)))",
            sys.argv[1],
        ],
        text=True,
    )
except (OSError, subprocess.CalledProcessError):
    fail("Ruby YAML parser is required for the pipeline contract")
value = json.loads(encoded)
steps = value.get("steps") if isinstance(value, dict) else None
if not isinstance(steps, list):
    fail("steps are missing")
if not all(isinstance(step, dict) and isinstance(step.get("key"), str) for step in steps):
    fail("every top-level step must have one explicit key; global waits are prohibited")
keyed = {step["key"]: step for step in steps}
if len(keyed) != len(steps):
    fail("step keys must be unique")

required = {
    "public-smoke",
    "public-nightly",
    "public-release",
    "sdk-swift-required",
    "public-cli-linux-factory",
    "public-cli-linux-x64-native-smoke",
    "public-cli-linux-aarch64-native-smoke",
    "public-cli-macos-arm64-native-smoke",
    "public-cli-macos-x64-runtime-producer",
    "public-cli-macos-x64-native-smoke",
    "public-cli-windows-x64-native-smoke",
    "github-release-candidate",
    "github-release-assets",
    "semantic-model-archives",
    "semantic-coreml-archive",
    "semantic-runtime-linux-cuda12",
    "semantic-runtime-windows-ml",
    "semantic-runtime-portable",
    "public-cli-macos-arm64-release-pair-qualification",
    "public-cli-macos-x64-release-pair-qualification",
    "semantic-release-handoff",
}
if set(keyed) != required:
    fail(
        f"unexpected step keys: missing={sorted(required-set(keyed))} "
        f"extra={sorted(set(keyed)-required)}"
    )

ordinary_condition = (
    'build.source != "schedule" && '
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") != "1" && '
    'build.env("CTX_PUBLIC_SEMANTIC_ASSET_MATRIX") != "1"'
)
nightly_condition = (
    'build.source == "schedule" && '
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") != "1" && '
    'build.env("CTX_PUBLIC_SEMANTIC_ASSET_MATRIX") != "1"'
)
core_release_condition = (
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1" && '
    'build.branch == "main" && build.pull_request.id == null'
)
core_native_condition = (
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1" && '
    'build.env("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX") == "1" && '
    'build.branch == "main" && build.pull_request.id == null'
)
semantic_condition = (
    'build.env("CTX_PUBLIC_SEMANTIC_ASSET_MATRIX") == "1" && '
    'build.branch == "main" && build.pull_request.id == null'
)
github_release_condition = (
    'build.env("CTX_PUBLIC_CLI_ARTIFACT_MATRIX") == "1" && '
    'build.env("CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX") == "1" && '
    'build.env("CTX_PUBLIC_SEMANTIC_ASSET_MATRIX") == "1" && '
    'build.branch == "main" && build.pull_request.id == null'
)

for key, mode, condition in (
    ("public-smoke", "ci", ordinary_condition),
    ("public-nightly", "nightly", nightly_condition),
    ("public-release", "release", core_release_condition),
):
    step = keyed[key]
    if step.get("command", "").strip() != (
        f"bash scripts/buildkite-public-ci.sh --mode={mode}"
    ):
        fail(f"{key} does not own the exact {mode} validation route")
    if step.get("if") != condition:
        fail(f"{key} has the wrong non-overlapping route condition")

swift = keyed["sdk-swift-required"]
if swift.get("if") != ordinary_condition:
    fail("Swift SDK qualification must remain on ordinary CI, outside release graphs")
if swift.get("command", "").strip() != (
    "swift --version\n"
    "CTX_SDK_RUN_LOCAL_SMOKE=0 bash scripts/check-sdks.sh "
    "--groups=swift --required-groups=swift"
):
    fail("Swift SDK qualification must invoke its exact fail-closed command")
if swift.get("depends_on") is not None or swift.get("soft_fail") or swift.get("skip"):
    fail("Swift SDK qualification must remain an independent required ordinary-CI step")
if swift.get("agents") != {
    "queue": "ctx-release-macos-arm64",
    "os": "darwin",
    "arch": "arm64",
}:
    fail("Swift SDK qualification has the wrong native runner")
if (
    swift.get("concurrency") != 1
    or swift.get("concurrency_group")
    != "ctx/sdk-swift-required/ctx-release-macos-arm64"
    or swift.get("timeout_in_minutes") != 30
):
    fail("Swift SDK qualification lost its bounded host contract")

linux_x64_selector = {
    "queue": "release-linux-managed",
    "ctx-runner-class": "release-linux-control",
    "ctx-release-os": "ubuntu-24.04",
    "ctx-release-nested-docker": "true",
    "os": "linux",
    "arch": "x86_64",
}
public_release_selector = {
    **linux_x64_selector,
    "ctx-release-task-bind": "true",
    "ctx-release-unshare-clone-fs": "true",
}
linux_x64_keys = {
    "public-cli-linux-factory",
    "public-cli-linux-x64-native-smoke",
    "github-release-candidate",
    "github-release-assets",
    "semantic-model-archives",
    "semantic-runtime-linux-cuda12",
    "semantic-runtime-portable",
    "semantic-release-handoff",
}
for key, step in keyed.items():
    agents = step.get("agents", {})
    if key == "public-release":
        if agents != public_release_selector:
            fail("public-release must require task-bind and exact CLONE_FS authority")
    elif key in linux_x64_keys:
        if agents != linux_x64_selector:
            fail(f"{key} must require the exact Linux x86_64 release selector")
    elif any(
        tag in agents
        for tag in (
            "ctx-release-os",
            "ctx-release-nested-docker",
            "ctx-release-task-bind",
            "ctx-release-unshare-clone-fs",
        )
    ):
        fail(f"{key} must not require Linux x86_64 release tags")

for key in ("public-cli-linux-x64-native-smoke", "public-cli-linux-aarch64-native-smoke"):
    if keyed[key].get("env") != {
        "CTX_PUBLIC_CLI_RUNTIME_AUTHORITY_BASELINE": "ubuntu-24.04"
    }:
        fail(f"{key} must require the Ubuntu 24.04 execution authority")

factory = keyed["public-cli-linux-factory"]
factory_command = factory.get("command", "")
if factory.get("if") != core_release_condition:
    fail("factory has the wrong Core release condition")
if (
    factory_command.count("build-public-candidate-on-linux.sh") != 1
    or factory_command.count("--macos-sdk") != 1
    or "--skip-runtimes" in factory_command
):
    fail("factory must invoke one five-target Core-only Linux construction route")
if "build-onnxruntime-sidecar.sh" in factory_command or "semantic" in factory_command.lower():
    fail("factory must not construct semantic assets")
if factory.get("depends_on") is not None or factory.get("secrets"):
    fail("factory must be independent and acquire signing values only at signing")
if factory.get("artifact_paths") != ["target/public-cli-artifacts/*"]:
    fail("factory must upload its complete Core candidate directory")

native = {
    "public-cli-linux-x64-native-smoke": (
        "linux-x64", "release-linux-managed", "linux", "x86_64"
    ),
    "public-cli-linux-aarch64-native-smoke": (
        "linux-aarch64", "linux-arm64", "linux", "arm64"
    ),
    "public-cli-macos-arm64-native-smoke": (
        "macos-arm64", "ctx-release-macos-arm64", "darwin", "arm64"
    ),
    "public-cli-macos-x64-native-smoke": (
        "macos-x64", "ctx-mac-gui-shared-x64", "darwin", "x86_64"
    ),
    "public-cli-windows-x64-native-smoke": (
        "windows-x64", "windows-x64", "windows", "x86_64"
    ),
}


def require_core_validator_call(key: str, platform: str, command: str) -> None:
    logical_command = re.sub(r"\\\n[ \t]*", " ", command)
    calls = []
    for line in logical_command.splitlines():
        normalized = " ".join(line.split())
        if "validate-public-cli-factory-artifact.sh" in normalized:
            calls.append(normalized)
    expected = (
        "scripts/validate-public-cli-factory-artifact.sh "
        f"{platform} target/public-cli-artifacts "
        f"target/public-cli-native-smoke/{platform}"
    )
    if calls != [expected]:
        fail(f"{key} must use the exact three-argument Core-only validator call")


for key, (platform, queue, os_name, arch) in native.items():
    step = keyed[key]
    if step.get("depends_on") != "public-cli-linux-factory":
        fail(f"{key} must depend only on the Core factory")
    if step.get("if") != core_native_condition:
        fail(f"{key} has the wrong Core native-release condition")
    agents = step.get("agents", {})
    if (agents.get("queue"), agents.get("os"), agents.get("arch")) != (
        queue,
        os_name,
        arch,
    ):
        fail(f"{key} has the wrong native runner")
    command = step.get("command", "")
    if command.count("download-linux-factory-artifacts.sh") != 1:
        fail(f"{key} must download exactly one Core factory artifact set")
    require_core_validator_call(key, platform, command)
    for marker in (
        "onnxruntime",
        "coreml",
        "semantic",
        "--runtime-archive",
        "--runtime-platform",
    ):
        if marker in command.lower():
            fail(f"{key} must not consume semantic model/runtime input ({marker})")
    if re.search(r"cargo (?:build|zigbuild)|bazelw run //:ctx_release", command):
        fail(f"{key} must never rebuild the candidate")
    if step.get("artifact_paths") != [
        f"target/public-cli-native-smoke/{platform}/candidate-smoke.json",
        f"target/public-cli-native-smoke/{platform}/ctx-{platform}.native-execution.json",
    ]:
        fail(f"{key} must upload only its exact Core validation evidence")

    output = f"target/public-cli-native-smoke/{platform}"
    mutated = command.replace(
        output,
        f"{output} companion-artifact managed-pair-envelope",
        1,
    )
    if mutated == command:
        fail(f"{key} validator arity mutation could not be constructed")
    try:
        require_core_validator_call(key, platform, mutated)
    except SystemExit:
        pass
    else:
        fail(f"{key} accepted a companion/pair-envelope validator mutation")

candidate = keyed["github-release-candidate"]
expected_candidate_dependencies = [
    "public-release",
    "public-cli-linux-factory",
    "public-cli-linux-x64-native-smoke",
    "public-cli-linux-aarch64-native-smoke",
    "public-cli-macos-arm64-native-smoke",
    "public-cli-macos-x64-native-smoke",
    "public-cli-windows-x64-native-smoke",
]
if candidate.get("depends_on") != expected_candidate_dependencies:
    fail("Core candidate staging has the wrong strict dependency set")
if candidate.get("if") != core_native_condition:
    fail("Core candidate staging has the wrong release condition")
if candidate.get("allow_dependency_failure") or candidate.get("soft_fail"):
    fail("Core candidate staging must fail closed")
candidate_command = candidate.get("command", "")
if (
    candidate_command.count('download-linux-factory-artifacts.sh "*"') != 1
    or candidate_command.count("stage-github-release-assets.sh") != 1
    or candidate_command.count("CTX_PUBLIC_RELEASE_SOURCE_COMMIT") != 1
    or "target/public-cli-native-smoke" not in candidate_command
    or "mv target/public-cli-native-smoke" in candidate_command
):
    fail(
        "Core candidate staging must keep the sealed factory immutable, "
        "consume separate native proofs, and bind HEAD"
    )
if "target/github-core-release-assets" not in candidate_command:
    fail("Core candidate staging must publish a Core-only handoff")
for marker in ("onnxruntime", "coreml", "semantic", "sdk-swift-required"):
    if marker in candidate_command.lower():
        fail(f"Core candidate staging must not consume {marker}")
for proof in native.values():
    platform = proof[0]
    if f"ctx-{platform}.native-execution.json" not in candidate_command:
        fail(f"Core candidate staging must consume native {platform} proof")

semantic_keys = {
    "public-cli-macos-x64-runtime-producer",
    "semantic-model-archives",
    "semantic-coreml-archive",
    "semantic-runtime-linux-cuda12",
    "semantic-runtime-windows-ml",
    "semantic-runtime-portable",
    "semantic-release-handoff",
}
for key in semantic_keys:
    if keyed[key].get("if") != semantic_condition:
        fail(f"{key} must use the independent Semantic matrix condition")

portable = keyed["semantic-runtime-portable"]
portable_command = portable.get("command", "")
if portable.get("depends_on") is not None:
    fail("portable Semantic runtime construction must be independent")
for platform in ("linux-x64", "linux-aarch64", "macos-arm64"):
    if platform not in portable_command:
        fail(f"portable Semantic runtime construction is missing {platform}")
if "windows-x64" in portable_command:
    fail("portable Semantic runtime construction must not build Windows CPU runtime")
if (
    portable_command.count("build-onnxruntime-sidecar.sh") != 1
    or portable_command.count("stage-github-release-assets.sh") != 1
    or "public-cli-linux-factory" in portable_command
):
    fail("portable Semantic runtime construction must own its runtime loop")
if portable.get("artifact_paths") != [
    "target/public-cli-artifacts/ctx-onnxruntime-linux-x64*",
    "target/public-cli-artifacts/ctx-onnxruntime-linux-aarch64*",
    "target/public-cli-artifacts/ctx-onnxruntime-macos-arm64*",
]:
    fail("portable Semantic runtime construction must upload only Unix CPU runtimes")

windows_runtime = keyed["semantic-runtime-windows-ml"]
windows_runtime_command = windows_runtime.get("command", "")
if (
    windows_runtime_command.count("build-onnxruntime-sidecar.sh") != 2
    or "build-onnxruntime-sidecar.sh windows-x64\n" not in windows_runtime_command
    or "build-onnxruntime-sidecar.sh windows-x64-windowsml" not in windows_runtime_command
):
    fail("Windows Semantic producer must qualify both CPU ONNX Runtime and Windows ML")
for expected_path in (
    "target/public-cli-artifacts/ctx-onnxruntime-windows-x64.zip",
    "target/public-cli-artifacts/ctx-onnxruntime-windows-x64.zip.sha256",
):
    if expected_path not in windows_runtime.get("artifact_paths", []):
        fail(f"Windows Semantic producer does not upload {expected_path}")

macos_x64_runtime = keyed["public-cli-macos-x64-runtime-producer"]
if (
    "build-onnxruntime-sidecar.sh macos-x64" not in macos_x64_runtime.get("command", "")
    or "stage-github-release-assets.sh --transcode-runtime macos-x64"
    not in macos_x64_runtime.get("command", "")
    or macos_x64_runtime.get("depends_on") is not None
):
    fail("macos-x64 Semantic runtime producer must remain independent")
if "download-linux-factory-artifacts.sh" in macos_x64_runtime.get("command", ""):
    fail("macos-x64 Semantic runtime producer must not consume Core artifacts")

release_pairs = {
    "public-cli-macos-arm64-release-pair-qualification": (
        "macos-arm64",
        "semantic-runtime-portable",
        "ctx-release-macos-arm64",
        "arm64",
        "ctx-public-cli-macos-arm64-release-pair",
    ),
    "public-cli-macos-x64-release-pair-qualification": (
        "macos-x64",
        "public-cli-macos-x64-runtime-producer",
        "ctx-mac-gui-shared-x64",
        "x86_64",
        "ctx-public-cli-macos-x64-release-pair",
    ),
}


def validate_release_pair(
    key: str,
    platform: str,
    runtime_producer: str,
    queue: str,
    arch: str,
    concurrency_group: str,
    pair: dict,
) -> None:
    if pair.get("depends_on") != ["github-release-candidate", runtime_producer]:
        fail(f"{key} must join only the sealed Core candidate and its runtime producer")
    if pair.get("if") != github_release_condition:
        fail(f"{key} has the wrong final-release condition")
    if any(field in pair for field in ("allow_dependency_failure", "soft_fail", "skip")):
        fail(f"{key} must fail closed")
    if pair.get("agents") != {"queue": queue, "os": "darwin", "arch": arch}:
        fail(f"{key} has the wrong authoritative native macOS runner")
    expected_env = (
        {"CTX_RELEASE_MACOS_X64_KVM_RUNNER_ID": "ctx-mac-gui-shared-x64"}
        if platform == "macos-x64"
        else None
    )
    if pair.get("env") != expected_env:
        fail(f"{key} has the wrong macOS qualification runner identity")
    if (
        pair.get("concurrency") != 1
        or pair.get("concurrency_group") != concurrency_group
        or pair.get("timeout_in_minutes") != 180
    ):
        fail(f"{key} lost its bounded native qualification contract")
    receipt = (
        f"target/macos-release-pair-qualification/ctx-{platform}.release-pair.sha256"
    )
    if pair.get("artifact_paths") != [receipt]:
        fail(f"{key} must publish only its exact gate-owned digest receipt")

    command = pair.get("command", "")
    logical_command = " ".join(command.replace("\\\n", " ").split())
    if "--include-retried-jobs" in command:
        fail(f"{key} must never consume historical retry artifacts")
    if re.search(r"BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=(?!false\b)", command):
        fail(f"{key} must set retry history exclusion exactly to false")

    def download(pattern: str, step: str) -> str:
        return (
            "BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=false "
            f'buildkite-agent artifact download "{pattern}" . --step {step}'
        )

    expected_commands = [
        "mkdir -p target/github-core-release-assets target/public-cli-artifacts",
        download(f"target/github-core-release-assets/ctx-{platform}*", "github-release-candidate"),
        download(
            f"target/public-cli-artifacts/ctx-onnxruntime-{platform}.tar.gz*",
            runtime_producer,
        ),
    ]
    for sidecar in (
        "signing.json",
        "attestation.json",
        "attestation.cms",
        "release-attestation.json",
        "release-attestation.cms",
        "notary-submit.json",
    ):
        expected_commands.append(
            download(
                f"target/public-cli-artifacts/ctx-onnxruntime-{platform}.{sidecar}",
                runtime_producer,
            )
        )
    cli = f"target/github-core-release-assets/ctx-{platform}"
    runtime = f"target/public-cli-artifacts/ctx-onnxruntime-{platform}.tar.gz"
    wrapper = f"scripts/qualify-macos-release-pair.sh {platform} {cli} {runtime} {receipt}"
    expected_commands.append(wrapper)
    actual_commands = [
        " ".join(line.split())
        for line in command.replace("\\\n", " ").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    if actual_commands != expected_commands:
        fail(
            f"{key} must use the canonical unmasked download sequence with "
            "the pair wrapper as its final unconditional simple command"
        )
    for forbidden in (
        "check-macos-release-signing.sh",
        "verify-macos-release-attestation.sh",
        "smoke-daemon-semantic-release.sh",
        "--coreml",
        "download-linux-factory-artifacts.sh",
    ):
        if forbidden in command:
            fail(f"{key} must delegate qualification only to the pair wrapper ({forbidden})")
    if "download-linux-factory-artifacts.sh" in command or "--coreml" in command:
        fail(f"{key} must consume only the final Core CLI/runtime pair")
    if re.search(r"cargo (?:build|zigbuild)|bazelw run //:ctx_release", command):
        fail(f"{key} must only qualify downloaded release inputs, never rebuild them")


for key, (platform, runtime_producer, queue, arch, concurrency_group) in release_pairs.items():
    pair = keyed[key]
    validate_release_pair(
        key, platform, runtime_producer, queue, arch, concurrency_group, pair
    )
    for old, new in (
        (f"--step {runtime_producer}", "--step public-cli-linux-factory"),
        (f"target/github-core-release-assets/ctx-{platform}*", "target/wrong/ctx"),
        ("notary-submit.json", "missing-notary-submit.json"),
        ("--step github-release-candidate", "--step github-release-candidate --include-retried-jobs"),
        ("scripts/qualify-macos-release-pair.sh", "if true; then scripts/qualify-macos-release-pair.sh"),
        ("scripts/qualify-macos-release-pair.sh", "exit 0\nscripts/qualify-macos-release-pair.sh"),
        ("scripts/qualify-macos-release-pair.sh", "scripts/qualify-macos-release-pair.sh || true"),
        ("BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=false", "BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=true"),
    ):
        mutated = copy.deepcopy(pair)
        mutated_command = mutated["command"].replace(old, new, 1)
        if mutated_command == mutated["command"]:
            fail(f"{key} release-pair negative mutation could not be constructed")
        mutated["command"] = mutated_command
        try:
            validate_release_pair(
                key, platform, runtime_producer, queue, arch, concurrency_group, mutated
            )
        except SystemExit:
            pass
        else:
            fail(f"{key} accepted release-pair mutation: {old} -> {new}")
    for field in ("allow_dependency_failure", "soft_fail", "skip"):
        mutated = copy.deepcopy(pair)
        mutated[field] = True
        try:
            validate_release_pair(
                key, platform, runtime_producer, queue, arch, concurrency_group, mutated
            )
        except SystemExit:
            pass
        else:
            fail(f"{key} accepted release-pair fail-open field: {field}")
    if platform == "macos-x64":
        mutated = copy.deepcopy(pair)
        mutated["env"] = {"CTX_RELEASE_MACOS_X64_KVM_RUNNER_ID": "wrong-runner"}
        try:
            validate_release_pair(
                key, platform, runtime_producer, queue, arch, concurrency_group, mutated
            )
        except SystemExit:
            pass
        else:
            fail(f"{key} accepted a mutated x64 KVM runner identity")

github_release = keyed["github-release-assets"]
expected_github_dependencies = [
    "github-release-candidate",
    "semantic-runtime-portable",
    "public-cli-macos-x64-runtime-producer",
    "semantic-runtime-windows-ml",
    "public-cli-macos-arm64-release-pair-qualification",
    "public-cli-macos-x64-release-pair-qualification",
]
if github_release.get("depends_on") != expected_github_dependencies:
    fail("final GitHub assembly has the wrong independent producer set")
if github_release.get("if") != github_release_condition:
    fail("final GitHub assembly has the wrong release condition")
if github_release.get("allow_dependency_failure") or github_release.get("soft_fail"):
    fail("final GitHub assembly must fail closed")
github_release_command = github_release.get("command", "")
def assembly_download(pattern: str, step: str) -> str:
    return (
        "BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=false "
        f'buildkite-agent artifact download "{pattern}" . --step {step}'
    )


expected_assembly_commands = [
    "mkdir -p target/public-cli-artifacts target/macos-release-pair-qualification",
    assembly_download("target/github-core-release-assets/*", "github-release-candidate"),
    assembly_download(
        "target/public-cli-artifacts/ctx-onnxruntime-linux-x64.tar.gz*",
        "semantic-runtime-portable",
    ),
    assembly_download(
        "target/public-cli-artifacts/ctx-onnxruntime-linux-aarch64.tar.gz*",
        "semantic-runtime-portable",
    ),
    assembly_download(
        "target/public-cli-artifacts/ctx-onnxruntime-macos-arm64.tar.gz*",
        "semantic-runtime-portable",
    ),
    assembly_download(
        "target/public-cli-artifacts/ctx-onnxruntime-macos-x64.tar.gz*",
        "public-cli-macos-x64-runtime-producer",
    ),
    assembly_download(
        "*ctx-onnxruntime-windows-x64.zip*", "semantic-runtime-windows-ml"
    ),
    assembly_download(
        "target/macos-release-pair-qualification/ctx-macos-arm64.release-pair.sha256",
        "public-cli-macos-arm64-release-pair-qualification",
    ),
    assembly_download(
        "target/macos-release-pair-qualification/ctx-macos-x64.release-pair.sha256",
        "public-cli-macos-x64-release-pair-qualification",
    ),
    (
        "scripts/assemble-github-release-assets.sh "
        "target/github-core-release-assets target/public-cli-artifacts "
        "target/github-release-assets target/macos-release-pair-qualification"
    ),
]

def validate_assembly_command(command: str) -> None:
    if "--include-retried-jobs" in command or re.search(
        r"BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=(?!false\b)", command
    ):
        fail("final GitHub assembly must never consume historical retry artifacts")
    actual_commands = [
        " ".join(line.split())
        for line in command.replace("\\\n", " ").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    if actual_commands != expected_assembly_commands:
        fail(
            "final GitHub assembly must join only current-attempt Core/five-runtime "
            "handoffs and both gate-owned pair receipts"
        )


validate_assembly_command(github_release_command)
for forbidden in ("multilingual-e5", "cuda12", "windowsml"):
    if forbidden in github_release_command.lower():
        fail(f"final GitHub assembly unexpectedly consumes {forbidden}")
if github_release.get("artifact_paths") != ["target/github-release-assets/*"]:
    fail("final GitHub assembly must upload only the complete public asset set")
for old, new in (
    (
        "target/macos-release-pair-qualification/ctx-macos-arm64.release-pair.sha256",
        "target/macos-release-pair-qualification/missing.release-pair.sha256",
    ),
    (
        "BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=false",
        "BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=true",
    ),
    (
        "target/macos-release-pair-qualification",
        "target/unqualified-release-pair-receipts",
    ),
):
    mutated = copy.deepcopy(github_release)
    mutated_command = mutated["command"].replace(old, new, 1)
    if mutated_command == mutated["command"]:
        fail(f"final assembly negative mutation could not be constructed: {old}")
    try:
        validate_assembly_command(mutated_command)
    except SystemExit:
        pass
    else:
        fail(f"final GitHub assembly accepted mutation: {old} -> {new}")

handoff = keyed["semantic-release-handoff"]
expected_semantic_dependencies = [
    "semantic-model-archives",
    "semantic-coreml-archive",
    "semantic-runtime-linux-cuda12",
    "semantic-runtime-windows-ml",
    "semantic-runtime-portable",
    "public-cli-macos-x64-runtime-producer",
]
if handoff.get("depends_on") != expected_semantic_dependencies:
    fail("Semantic handoff has the wrong independent producer set")
handoff_command = handoff.get("command", "")
if handoff_command.count("--step semantic-runtime-portable") != 3:
    fail("Semantic handoff must gather its three portable CPU runtime families")
for forbidden in (
    "public-release",
    "public-cli-linux-factory",
    "public-cli-macos-arm64-release-pair-qualification",
    "public-cli-macos-x64-release-pair-qualification",
    "sdk-swift-required",
):
    if forbidden in handoff_command:
        fail(f"Semantic handoff must not consume Core/SDK input ({forbidden})")

core_keys = {
    "public-release",
    "public-cli-linux-factory",
    *native.keys(),
    "github-release-candidate",
}
for key in core_keys:
    dependencies = keyed[key].get("depends_on", [])
    if isinstance(dependencies, str):
        dependencies = [dependencies]
    if set(dependencies) & (semantic_keys | {"sdk-swift-required"}):
        fail(f"{key} crosses from the Core graph into SDK/Semantic work")
for key in semantic_keys:
    dependencies = keyed[key].get("depends_on", [])
    if isinstance(dependencies, str):
        dependencies = [dependencies]
    if set(dependencies) & (core_keys | {"sdk-swift-required"}):
        fail(f"{key} crosses from the Semantic graph into Core/SDK work")

for step in steps:
    command = str(step.get("command", ""))
    match = re.search(
        r"(?<![$])[$](?:[{][^}\n]+[}]|[A-Za-z_][A-Za-z0-9_]*)", command
    )
    if match:
        fail(f"{step.get('key')} exposes {match.group(0)} to Buildkite interpolation")

print(
    "Buildkite release pipeline: independent Core/SDK/Semantic graphs, "
    "five exact-byte Core validators, two authoritative macOS release-pair gates"
)
PY

python3 scripts/tests/buildkite-windowsml-artifact-path-test.py "${pipeline}"
bash scripts/tests/buildkite-public-ci-cache-test.sh
bash scripts/tests/check-sdks-required-groups-test.sh
python3 scripts/check-sdk-ci-pipeline.py \
  scripts/buildkite-public-ci.sh scripts/check-sdks.sh

"""Target-configured public CLI release packaging routes."""

load(":release_inventory.bzl", "PUBLIC_RELEASE_ROUTES")

ReleaseRouteInfo = provider(
    doc = "Selected public release graph identity.",
    fields = {
        "available": "Whether the route has an owned build graph.",
        "platform": "Bazel target platform label.",
        "target_id": "Release matrix target id.",
        "target_triple": "Rust target triple.",
    },
)

def _route_transition_impl(_settings, attr):
    route = PUBLIC_RELEASE_ROUTES[attr.target_id]
    return {
        "//command_line_option:platforms": route[0],
        "//tools/bazel:windows_gnu_release": attr.target_id == "windows-x64",
    }

_route_transition = transition(
    implementation = _route_transition_impl,
    inputs = [],
    outputs = [
        "//command_line_option:platforms",
        "//tools/bazel:windows_gnu_release",
    ],
)

def _launcher_content(target_id):
    llvm_readobj_argument = ""
    if target_id == "windows-x64":
        llvm_readobj_argument = """  --declared-llvm-readobj-runfile "${{route_root}}/llvm-readobj.exe" \\
"""
    return """#!/usr/bin/env bash
set -euo pipefail

runfiles_root="${{RUNFILES_DIR:-$0.runfiles}}"
workspace="${{TEST_WORKSPACE:-_main}}"

resolve_main_runfile() {{
  local key="$1"
  local candidate
  for candidate in \
    "${{runfiles_root}}/${{workspace}}/${{key}}" \
    "${{runfiles_root}}/_main/${{key}}"; do
    if [[ -e "${{candidate}}" ]]; then
      printf '%s\\n' "${{candidate}}"
      return 0
    fi
  done
  if [[ -n "${{RUNFILES_MANIFEST_FILE:-}}" ]]; then
    local logical physical
    while IFS=' ' read -r logical physical; do
      case "${{logical}}" in
        "${{workspace}}/${{key}}"|"_main/${{key}}")
          printf '%s\\n' "${{physical}}"
          return 0
          ;;
      esac
    done <"${{RUNFILES_MANIFEST_FILE}}"
  fi
  printf 'error: declared release runfile is unavailable: %s\\n' "${{key}}" >&2
  return 1
}}

route_root="ctx_release_routes/{target_id}"
packager="$(resolve_main_runfile "${{route_root}}/packager")"
export RUNFILES_DIR="${{runfiles_root}}"
if [[ -z "${{RUNFILES_MANIFEST_FILE:-}}" ]]; then
  for manifest in "$0.runfiles_manifest" "${{runfiles_root}}/MANIFEST"; do
    if [[ -f "${{manifest}}" ]]; then
      export RUNFILES_MANIFEST_FILE="${{manifest}}"
      break
    fi
  done
fi
exec "${{packager}}" \
  --declared-advisory-gate-runfile "${{route_root}}/advisory-gate" \
  --declared-artifact-runfile "${{route_root}}/artifact" \
  --declared-rustc-runfile "${{route_root}}/rustc" \
{llvm_readobj_argument}\
  --declared-sbom-tool-runfile "${{route_root}}/sbom-tool" \
  --declared-sbom-inventory-runfile "${{route_root}}/sbom-inventory.txt" \
  --declared-license-materials-runfile "${{route_root}}/license-materials.txt" \
  --declared-cargo-lock-runfile "${{route_root}}/Cargo.lock" \
  --declared-target-matrix-runfile "${{route_root}}/release-targets-v1.json" \
  --declared-target "{target_id}" \
  "$@"
""".format(
        llvm_readobj_argument = llvm_readobj_argument,
        target_id = target_id,
    )

def _release_route_impl(ctx):
    route = PUBLIC_RELEASE_ROUTES[ctx.attr.target_id]
    launcher = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(
        output = launcher,
        content = _launcher_content(ctx.attr.target_id),
        is_executable = True,
    )

    route_root = "ctx_release_routes/{}".format(ctx.attr.target_id)
    files = [
        ctx.executable.advisory_gate,
        ctx.file.artifact,
        ctx.file.cargo_lock,
        ctx.file.license_materials,
        ctx.file.rustc,
        ctx.file.sbom_inventory,
        ctx.file.target_matrix,
        ctx.executable.sbom_tool,
    ]
    symlinks = {
        "{}/advisory-gate".format(route_root): ctx.executable.advisory_gate,
        "{}/Cargo.lock".format(route_root): ctx.file.cargo_lock,
        "{}/artifact".format(route_root): ctx.file.artifact,
        "{}/license-materials.txt".format(route_root): ctx.file.license_materials,
        "{}/packager".format(route_root): ctx.executable.packager,
        "{}/release-targets-v1.json".format(route_root): ctx.file.target_matrix,
        "{}/rustc".format(route_root): ctx.file.rustc,
        "{}/sbom-tool".format(route_root): ctx.executable.sbom_tool,
        "{}/sbom-inventory.txt".format(route_root): ctx.file.sbom_inventory,
    }
    if ctx.file.llvm_readobj:
        files.append(ctx.file.llvm_readobj)
        symlinks["{}/llvm-readobj.exe".format(route_root)] = ctx.file.llvm_readobj
    runfiles = ctx.runfiles(
        files = files,
        symlinks = symlinks,
    )
    runfiles = runfiles.merge(
        ctx.attr.license_materials[0][DefaultInfo].default_runfiles,
    )
    runfiles = runfiles.merge(ctx.attr.advisory_gate[DefaultInfo].default_runfiles)
    runfiles = runfiles.merge(ctx.attr.packager[DefaultInfo].default_runfiles)
    runfiles = runfiles.merge(ctx.attr.sbom_tool[DefaultInfo].default_runfiles)
    return [
        DefaultInfo(executable = launcher, runfiles = runfiles),
        ReleaseRouteInfo(
            available = True,
            platform = route[0],
            target_id = ctx.attr.target_id,
            target_triple = route[1],
        ),
    ]

_release_route = rule(
    implementation = _release_route_impl,
    executable = True,
    attrs = {
        "advisory_gate": attr.label(
            cfg = "exec",
            executable = True,
            mandatory = True,
        ),
        "artifact": attr.label(
            allow_single_file = True,
            cfg = _route_transition,
            mandatory = True,
        ),
        "cargo_lock": attr.label(allow_single_file = True, mandatory = True),
        "license_materials": attr.label(
            allow_single_file = True,
            cfg = _route_transition,
            mandatory = True,
        ),
        "llvm_readobj": attr.label(allow_single_file = True),
        "packager": attr.label(
            cfg = "exec",
            executable = True,
            mandatory = True,
        ),
        "rustc": attr.label(
            allow_single_file = True,
            cfg = _route_transition,
            mandatory = True,
        ),
        "sbom_inventory": attr.label(
            allow_single_file = True,
            cfg = _route_transition,
            mandatory = True,
        ),
        "sbom_tool": attr.label(
            cfg = "exec",
            executable = True,
            mandatory = True,
        ),
        "target_id": attr.string(mandatory = True, values = sorted(PUBLIC_RELEASE_ROUTES.keys())),
        "target_matrix": attr.label(allow_single_file = True, mandatory = True),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    },
)

def public_cli_release_route(
        name,
        target_id,
        advisory_gate,
        artifact,
        rustc,
        packager,
        sbom_inventory,
        license_materials,
        sbom_tool = "//:release_sbom"):
    """Declares one exact target-configured release route."""
    llvm_readobj = None
    if target_id == "windows-x64":
        llvm_readobj = "@ctx_llvm_mingw//:bin/llvm-readobj.exe"
    _release_route(
        name = name,
        advisory_gate = advisory_gate,
        artifact = artifact,
        cargo_lock = "//:release_cargo_lock",
        license_materials = license_materials,
        llvm_readobj = llvm_readobj,
        packager = packager,
        rustc = rustc,
        sbom_inventory = sbom_inventory,
        sbom_tool = sbom_tool,
        target_id = target_id,
        target_matrix = "//:release_target_matrix",
        tags = ["manual", "release-gate", "release-tool"],
    )

def _advisory_launcher_content(target_id):
    return """#!/usr/bin/env bash
set -euo pipefail

: "${{BUILD_WORKSPACE_DIRECTORY:?release advisory gate requires a source workspace}}"
: "${{CTX_RELEASE_ADVISORY_RECEIPT_DIR:?set CTX_RELEASE_ADVISORY_RECEIPT_DIR}}"
: "${{CTX_OSV_SCANNER:?set CTX_OSV_SCANNER to the pinned scanner executable}}"
: "${{CTX_OSV_DATABASE_DIR:?set CTX_OSV_DATABASE_DIR}}"
: "${{CTX_OSV_DATABASE_METADATA:?set CTX_OSV_DATABASE_METADATA}}"

runfiles_root="${{RUNFILES_DIR:-$0.runfiles}}"
workspace="${{TEST_WORKSPACE:-_main}}"

resolve_main_runfile() {{
  local key="$1"
  local candidate
  for candidate in \
    "${{runfiles_root}}/${{workspace}}/${{key}}" \
    "${{runfiles_root}}/_main/${{key}}"; do
    if [[ -e "${{candidate}}" ]]; then
      printf '%s\\n' "${{candidate}}"
      return 0
    fi
  done
  if [[ -n "${{RUNFILES_MANIFEST_FILE:-}}" ]]; then
    local logical physical
    while IFS=' ' read -r logical physical; do
      case "${{logical}}" in
        "${{workspace}}/${{key}}"|"_main/${{key}}")
          printf '%s\\n' "${{physical}}"
          return 0
          ;;
      esac
    done <"${{RUNFILES_MANIFEST_FILE}}"
  fi
  printf 'error: declared advisory runfile is unavailable: %s\\n' "${{key}}" >&2
  return 1
}}

route_root="ctx_release_advisory/{target_id}"
script="$(resolve_main_runfile "${{route_root}}/gate")"
inventory="$(resolve_main_runfile "${{route_root}}/cargo-inventory.txt")"
policy="$(resolve_main_runfile "${{route_root}}/policy.json")"
exceptions="$(resolve_main_runfile "${{route_root}}/exceptions.json")"
export RUNFILES_DIR="${{runfiles_root}}"
if [[ -z "${{RUNFILES_MANIFEST_FILE:-}}" ]]; then
  for manifest in "$0.runfiles_manifest" "${{runfiles_root}}/MANIFEST"; do
    if [[ -f "${{manifest}}" ]]; then
      export RUNFILES_MANIFEST_FILE="${{manifest}}"
      break
    fi
  done
fi
exec "${{script}}" \
  --repo-root "${{BUILD_WORKSPACE_DIRECTORY}}" \
  --policy "${{policy}}" \
  --exceptions "${{exceptions}}" \
  --database-root "${{CTX_OSV_DATABASE_DIR}}" \
  --database-metadata "${{CTX_OSV_DATABASE_METADATA}}" \
  --scanner "${{CTX_OSV_SCANNER}}" \
  --cargo-inventory "${{inventory}}" \
  --target-id "{target_id}" \
  --output "${{CTX_RELEASE_ADVISORY_RECEIPT_DIR}}/public-{target_id}.json"
""".format(target_id = target_id)

def _release_advisory_gate_impl(ctx):
    launcher = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(
        output = launcher,
        content = _advisory_launcher_content(ctx.attr.target_id),
        is_executable = True,
    )
    route_root = "ctx_release_advisory/{}".format(ctx.attr.target_id)
    runfiles = ctx.runfiles(
        files = [
            ctx.file.exceptions,
            ctx.file.inventory,
            ctx.file.policy,
            ctx.executable.script,
        ],
        symlinks = {
            "{}/cargo-inventory.txt".format(route_root): ctx.file.inventory,
            "{}/exceptions.json".format(route_root): ctx.file.exceptions,
            "{}/gate".format(route_root): ctx.executable.script,
            "{}/policy.json".format(route_root): ctx.file.policy,
        },
    )
    runfiles = runfiles.merge(ctx.attr.script[DefaultInfo].default_runfiles)
    return [DefaultInfo(executable = launcher, runfiles = runfiles)]

_release_advisory_gate = rule(
    implementation = _release_advisory_gate_impl,
    executable = True,
    attrs = {
        "exceptions": attr.label(allow_single_file = True, mandatory = True),
        "inventory": attr.label(
            allow_single_file = True,
            cfg = _route_transition,
            mandatory = True,
        ),
        "policy": attr.label(allow_single_file = True, mandatory = True),
        "script": attr.label(
            cfg = "exec",
            executable = True,
            mandatory = True,
        ),
        "target_id": attr.string(mandatory = True, values = sorted(PUBLIC_RELEASE_ROUTES.keys())),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    },
)

def public_cli_release_advisory_gate(name, target_id, inventory):
    """Declares one target-configured, offline dependency-advisory gate."""
    _release_advisory_gate(
        name = name,
        exceptions = "//:release_advisory_exceptions",
        inventory = inventory,
        policy = "//:release_advisory_policy",
        script = "//:dependency_advisory_gate",
        target_id = target_id,
        tags = ["manual", "release-gate", "release-tool"],
    )

def public_cli_release_advisory_gates(name_prefix, inventory):
    """Declares the advisory gate for every owned public release route."""
    for target_id in sorted(PUBLIC_RELEASE_ROUTES.keys()):
        public_cli_release_advisory_gate(
            name = "{}_{}".format(name_prefix, target_id.replace("-", "_")),
            inventory = inventory,
            target_id = target_id,
        )

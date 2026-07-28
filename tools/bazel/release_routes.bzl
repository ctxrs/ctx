"""Target-configured public CLI release packaging routes."""

ReleaseRouteInfo = provider(
    doc = "Selected public release graph identity.",
    fields = {
        "available": "Whether the route has an owned build graph.",
        "platform": "Bazel target platform label.",
        "target_id": "Release matrix target id.",
        "target_triple": "Rust target triple.",
    },
)

_ROUTES = {
    "linux-x64": struct(
        platform = "//tools/bazel/platforms:release_linux_x64",
        triple = "x86_64-unknown-linux-gnu",
    ),
    "linux-arm64": struct(
        platform = "//tools/bazel/platforms:release_linux_arm64",
        triple = "aarch64-unknown-linux-gnu",
    ),
    "macos-arm64": struct(
        platform = "//tools/bazel/platforms:release_macos_arm64",
        triple = "aarch64-apple-darwin",
    ),
    "macos-x64": struct(
        platform = "//tools/bazel/platforms:release_macos_x64",
        triple = "x86_64-apple-darwin",
    ),
    "windows-x64": struct(
        platform = "//tools/bazel/platforms:release_windows_x64_gnu",
        triple = "x86_64-pc-windows-gnu",
    ),
    "freebsd-x64": struct(
        platform = "//tools/bazel/platforms:release_freebsd_x64",
        triple = "x86_64-unknown-freebsd",
    ),
}

def _route_transition_impl(_settings, attr):
    route = _ROUTES[attr.target_id]
    return {
        "//command_line_option:platforms": route.platform,
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
exec "${{packager}}" \
  --declared-artifact-runfile "${{route_root}}/artifact" \
  --declared-rustc-runfile "${{route_root}}/rustc" \
  --declared-sbom-inventory-runfile "${{route_root}}/sbom-inventory.txt" \
  --declared-license-materials-runfile "${{route_root}}/license-materials.txt" \
  --declared-cargo-lock-runfile "${{route_root}}/Cargo.lock" \
  --declared-target-matrix-runfile "${{route_root}}/release-targets-v1.json" \
  --declared-target "{target_id}" \
  "$@"
""".format(target_id = target_id)

def _release_route_impl(ctx):
    route = _ROUTES[ctx.attr.target_id]
    launcher = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.write(
        output = launcher,
        content = _launcher_content(ctx.attr.target_id),
        is_executable = True,
    )

    route_root = "ctx_release_routes/{}".format(ctx.attr.target_id)
    runfiles = ctx.runfiles(
        files = [
            ctx.file.artifact,
            ctx.file.cargo_lock,
            ctx.file.license_materials,
            ctx.file.rustc,
            ctx.file.sbom_inventory,
            ctx.file.target_matrix,
        ],
        symlinks = {
            "{}/Cargo.lock".format(route_root): ctx.file.cargo_lock,
            "{}/artifact".format(route_root): ctx.file.artifact,
            "{}/license-materials.txt".format(route_root): ctx.file.license_materials,
            "{}/packager".format(route_root): ctx.executable.packager,
            "{}/release-targets-v1.json".format(route_root): ctx.file.target_matrix,
            "{}/rustc".format(route_root): ctx.file.rustc,
            "{}/sbom-inventory.txt".format(route_root): ctx.file.sbom_inventory,
        },
    )
    runfiles = runfiles.merge(
        ctx.attr.license_materials[0][DefaultInfo].default_runfiles,
    )
    runfiles = runfiles.merge(ctx.attr.packager[DefaultInfo].default_runfiles)
    return [
        DefaultInfo(executable = launcher, runfiles = runfiles),
        ReleaseRouteInfo(
            available = True,
            platform = route.platform,
            target_id = ctx.attr.target_id,
            target_triple = route.triple,
        ),
    ]

_release_route = rule(
    implementation = _release_route_impl,
    executable = True,
    attrs = {
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
        "target_id": attr.string(mandatory = True, values = sorted(_ROUTES.keys())),
        "target_matrix": attr.label(allow_single_file = True, mandatory = True),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    },
)

def public_cli_release_route(
        name,
        target_id,
        artifact,
        rustc,
        packager,
        sbom_inventory,
        license_materials):
    """Declares one exact target-configured release route."""
    _release_route(
        name = name,
        artifact = artifact,
        cargo_lock = "//:release_cargo_lock",
        license_materials = license_materials,
        packager = packager,
        rustc = rustc,
        sbom_inventory = sbom_inventory,
        target_id = target_id,
        target_matrix = "//:release_target_matrix",
        tags = ["manual", "release-gate", "release-tool"],
    )

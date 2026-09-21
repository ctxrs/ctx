#!/usr/bin/env python3
"""Fail closed on the agent-integrations dependency and authority boundary."""

from __future__ import annotations

import pathlib
import re
import sys
import tomllib


PACKAGE = "ctx-agent-integrations"
EXPECTED_LOCAL_DEPS = {
    "ctx-attribution-model",
    "ctx-client-observability",
    "ctx-history-core",
}
ALLOWED_REVERSE_DEPENDENTS = {"ctx", "ctx-agent-application", "ctx-cli-presentation"}


def fail(message: str) -> None:
    raise SystemExit(f"agent integrations boundary check failed: {message}")


def manifest_dependencies(data: dict) -> set[str]:
    dependencies: set[str] = set()

    def collect(table: object) -> None:
        if not isinstance(table, dict):
            return
        for alias, value in table.items():
            if isinstance(value, dict):
                dependencies.add(value.get("package", alias))
            else:
                dependencies.add(alias)

    # Test-only integration harnesses may depend back on a production consumer
    # without adding a shipped-library edge. Keep the authority DAG scoped to
    # normal and build dependencies, matching Cargo's production resolution.
    for name in ("dependencies", "build-dependencies"):
        collect(data.get(name))
    for target in data.get("target", {}).values():
        if isinstance(target, dict):
            for name in ("dependencies", "build-dependencies"):
                collect(target.get(name))
    return dependencies


def find_cycle(graph: dict[str, set[str]]) -> list[str] | None:
    active: list[str] = []
    complete: set[str] = set()

    def visit(node: str) -> list[str] | None:
        if node in active:
            index = active.index(node)
            return active[index:] + [node]
        if node in complete:
            return None
        active.append(node)
        for dependency in sorted(graph.get(node, set())):
            cycle = visit(dependency)
            if cycle:
                return cycle
        active.pop()
        complete.add(node)
        return None

    for node in sorted(graph):
        cycle = visit(node)
        if cycle:
            return cycle
    return None


def rust_sources(crate_dir: pathlib.Path) -> list[pathlib.Path]:
    return sorted((crate_dir / "src").rglob("*.rs"))


def main() -> None:
    if len(sys.argv) != 4:
        fail("expected ROOT_CARGO_TOML INTEGRATIONS_CARGO_TOML INTEGRATIONS_BUILD")
    root_manifest = pathlib.Path(sys.argv[1]).resolve()
    integrations_manifest = pathlib.Path(sys.argv[2]).resolve()
    integrations_build = pathlib.Path(sys.argv[3]).resolve()
    root = root_manifest.parent
    workspace = tomllib.loads(root_manifest.read_text(encoding="utf-8"))["workspace"]
    manifests = [root / member / "Cargo.toml" for member in workspace["members"]]
    packages = {
        tomllib.loads(path.read_text(encoding="utf-8"))["package"]["name"]: path
        for path in manifests
    }
    if packages.get(PACKAGE) != integrations_manifest:
        fail("manifest is not the canonical workspace member")

    graph: dict[str, set[str]] = {name: set() for name in packages}
    for name, path in packages.items():
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        graph[name] = manifest_dependencies(data).intersection(packages)
    cycle = find_cycle(graph)
    if cycle:
        fail(f"workspace dependency cycle: {' -> '.join(cycle)}")
    if graph[PACKAGE] != EXPECTED_LOCAL_DEPS:
        fail(
            f"local dependencies are {sorted(graph[PACKAGE])}, "
            f"expected {sorted(EXPECTED_LOCAL_DEPS)}"
        )
    reverse = {name for name, dependencies in graph.items() if PACKAGE in dependencies}
    if reverse != ALLOWED_REVERSE_DEPENDENTS:
        fail(
            f"reverse dependents are {sorted(reverse)}, "
            f"expected {sorted(ALLOWED_REVERSE_DEPENDENTS)}"
        )

    bazel_local_deps = set(
        re.findall(r'"//crates/([^/:]+):lib"', integrations_build.read_text(encoding="utf-8"))
    )
    if bazel_local_deps != EXPECTED_LOCAL_DEPS:
        fail(
            f"Bazel local dependencies are {sorted(bazel_local_deps)}, "
            f"expected {sorted(EXPECTED_LOCAL_DEPS)}"
        )

    crate_dir = integrations_manifest.parent
    sources = rust_sources(crate_dir)
    forbidden_fragments = [
        'env!("CARGO_PKG_VERSION")',
        "AppConfig",
        "IntegrationTelemetry",
        "PublicEventV1",
        "local_usage",
        "ctx_attribution::",
        "ctx_attribution_index::",
        "ctx_repository_evidence::",
    ]
    for path in sources:
        body = path.read_text(encoding="utf-8")
        for fragment in forbidden_fragments:
            if fragment in body:
                fail(f"forbidden fragment {fragment!r} in {path.relative_to(root)}")

    stale_authorities = [
        "crates/ctx-cli/src/mcp/arguments.rs",
        "crates/ctx-cli/src/mcp/input.rs",
        "crates/ctx-cli/src/mcp/query_events.rs",
        "crates/ctx-cli/src/mcp/response.rs",
        "crates/ctx-cli/src/mcp/response_bound.rs",
        "crates/ctx-cli/src/mcp/show.rs",
        "crates/ctx-cli/src/integrations/mcp/format/json.rs",
        "crates/ctx-cli/src/integrations/mcp/format/toml.rs",
        "crates/ctx-cli/src/integrations/mcp/format/yaml.rs",
        "crates/ctx-cli/src/integrations/mcp/registry.rs",
        "crates/ctx-cli/src/integrations/slash_commands/application.rs",
        "crates/ctx-cli/src/skill/agents.rs",
        "crates/ctx-cli/src/skill/paths.rs",
        "crates/ctx-cli/src/skill/target.rs",
        "crates/ctx-cli/src/skill/install/application.rs",
    ]
    stale = [path for path in stale_authorities if (root / path).exists()]
    if stale:
        fail(f"stale pre-extraction authorities remain: {stale}")

    cli_mcp_operation = root / "crates/ctx-cli-presentation/src/integrations/mcp/operation.rs"
    duplicate_config_functions = re.findall(
        r"\bfn\s+(install_target|status_target|read_target_status|write_target)\b",
        cli_mcp_operation.read_text(encoding="utf-8"),
    )
    if duplicate_config_functions:
        fail(f"duplicate CLI MCP-config functions remain: {duplicate_config_functions}")

    print(
        f"agent integrations boundary is acyclic with local deps {sorted(graph[PACKAGE])}"
    )


if __name__ == "__main__":
    main()

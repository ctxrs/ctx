"""Implementation for the exact MCP attribution capability checker."""

from __future__ import annotations

import json
import re
import tomllib
from collections import Counter, defaultdict
from collections.abc import Mapping
from pathlib import Path
from typing import Any, Callable, Iterable
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

try:
    from .mcp_tool_call_attribution_boundary import (
        contract_path_violation,
        public_boundary_violation,
    )
except ImportError:
    from mcp_tool_call_attribution_boundary import (
        contract_path_violation,
        public_boundary_violation,
    )


REPO_ROOT = Path(__file__).resolve().parents[1]
SUPPORT_MATRIX_PATH = REPO_ROOT / "docs/provider-support-matrix.json"
CAPABILITY_PATH = REPO_ROOT / "docs/mcp-tool-call-attribution-capabilities.json"
PUBLIC_DOC_PATHS = (
    "docs/mcp-tool-call-attribution.md",
    "docs/mcp-tool-call-attribution-evidence.md",
    "docs/mcp-exchange-capture.md",
    "docs/provider-support.md",
    "docs/providers.md",
    "docs/cli-reference.md",
    "docs/contracts/json.md",
    "docs/event-queries.md",
    "docs/mcp.md",
    "docs/privacy-storage.md",
    "docs/search.md",
    "docs/sdks.md",
    "docs/storage.md",
)
ALLOWED_STATUSES = {"exact", "not-qualified", "excluded"}
EXACT_PROVIDERS = {"codex", "copilot_cli", "warp"}
EXPECTED_PROVIDER_STATUS_COUNTS = {"exact": 3, "not-qualified": 40, "excluded": 0}
EXPECTED_LANE_STATUS_COUNTS = {"exact": 3, "not-qualified": 48, "excluded": 1}
EXPECTED_COUNTS = {
    "providers": 43,
    "base_routes": 48,
    "capability_lanes": 52,
    "lane_statuses": EXPECTED_LANE_STATUS_COUNTS,
    "provider_statuses": EXPECTED_PROVIDER_STATUS_COUNTS,
}
CODEX_SESSION_ROUTE = ("codex", "native_import", "codex_session_jsonl_tree")
CODEX_HISTORY_ROUTE = ("codex", "native_import", "codex_history_jsonl")
CODEX_EXACT_SCHEMA = "codex-nativepath-jsonl-v0"
CODEX_EXACT_BOUND = {
    "kind": "unversioned",
    "generation": 1,
    "unknown_generations": "not-qualified",
}
CODEX_PUBLIC_SOURCE_COMMIT = "60c722e07514d46d980034319dfcbfe4e74e659f"
CODEX_NOT_QUALIFIED_VERSIONS = ("0.200.0", "0.201.0", "0.202.0")
EXPECTED_NOT_QUALIFIED_REASONS = {
    "codex": "route_mismatch",
    "grok_build": "no_server_field",
    "deepseek_harness": "no_server_field",
    "pi": "no_server_field",
    "claude_code": "lossy_composite",
    "open_code": "lossy_composite",
    "kilo": "lossy_composite",
    "mimocode": "lossy_composite",
    "kiro_cli": "exact_pair_transient_or_config",
    "crush": "lossy_composite",
    "goose": "exact_pair_transient_or_config",
    "lingma": "route_mismatch",
    "qoder": "lossy_composite",
    "codebuddy": "writer_version_unproven",
    "openclaw": "route_mismatch",
    "hermes": "lossy_composite",
    "nanoclaw": "no_unique_terminal_link",
    "astrbot": "no_server_field",
    "shelley": "no_server_field",
    "continue": "lossy_composite",
    "openhands": "route_mismatch",
    "antigravity_cli": "writer_version_unproven",
    "gemini_cli": "lossy_composite",
    "tabnine": "lossy_composite",
    "cursor": "no_unique_terminal_link",
    "zed": "lossy_composite",
    "factory_ai_droid": "lossy_composite",
    "qwen_code": "lossy_composite",
    "kimi_code_cli": "exact_pair_transient_or_config",
    "auggie": "lossy_composite",
    "junie": "lossy_composite",
    "firebender": "lossy_composite",
    "xopc": "no_server_field",
    "forgecode": "lossy_composite",
    "mux": "lossy_composite",
    "rovodev": "lossy_composite",
    "cline": "no_unique_terminal_link",
    "roo_code": "no_unique_terminal_link",
    "deepagents": "lossy_composite",
    "mistral_vibe": "lossy_composite",
    "fx": "no_server_field",
}
ROW_FIELDS = set(
    "provider_id route source_format source_schema producer_bound status detail evidence".split()
)
CHECK_FIELDS = set(
    "provider_id route source_format source_schema producer_bound implementation_source "
    "suite_id tests".split()
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
VERSION_RE = re.compile(r"^\d+(?:\.\d+){1,3}(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$")
PINNED_GITHUB_RE = re.compile(
    r"^https://github\.com/[^/]+/[^/]+/(?:blob|tree)/([0-9a-f]{40})"
    r"(?:/[^?#]+)?(?:#L\d+(?:-L\d+)?)?$"
)
PRODUCER_BOUND_GRAMMAR = {
    "allowed_kinds": ["unversioned", "source_commits", "versions", "hosted_boundary"],
    "unversioned_fields": ["kind", "generation", "unknown_generations"],
    "source_commits_fields": ["kind", "commits", "unknown_generations"],
    "versions_fields": [
        "kind",
        "versions",
        "ranges",
        "source_commits",
        "unknown_generations",
    ],
    "hosted_boundary_fields": ["kind", "unknown_generations"],
    "version_range_fields": ["minimum", "maximum"],
    "version_pattern": VERSION_RE.pattern,
    "source_commit_pattern": SHA_RE.pattern,
    "unknown_generations_by_status": {
        "exact": "not-qualified",
        "not-qualified": "not-qualified",
        "excluded": "excluded",
    },
    "exact_requires_source_commits": False,
}


class CapabilityError(Exception):
    pass


def fail(message: str) -> None:
    raise CapabilityError(message)


def _checker_source_role(relative: Path) -> str | None:
    tokens = frozenset(re.findall(r"[a-z0-9]+", relative.as_posix().lower()))
    if relative.parent == Path("scripts") and {"check", "docs"}.issubset(tokens):
        return "docs gate"
    if {"mcp", "tool", "call", "attribution"}.issubset(tokens):
        return "attribution test" if "tests" in relative.parts else "attribution checker"
    return None


def discover_public_checker_source_paths(repo_root: Path = REPO_ROOT) -> tuple[Path, ...]:
    scripts_root = repo_root / "scripts"
    if not scripts_root.is_dir():
        fail("public checker source root is missing")
    discovered: list[Path] = []
    roles: Counter[str] = Counter()
    for path in scripts_root.rglob("*"):
        if not path.is_file() or "__pycache__" in path.parts:
            continue
        relative = path.relative_to(repo_root)
        role = _checker_source_role(relative)
        if role is None:
            continue
        discovered.append(relative)
        roles[role] += 1
    required_roles = {
        "docs gate",
        "attribution checker",
        "attribution test",
    }
    if not required_roles.issubset(roles):
        fail(f"public checker source discovery is incomplete: roles={dict(roles)}")
    return tuple(sorted(discovered))


def validate_public_checker_sources(
    repo_root: Path = REPO_ROOT,
    source_overrides: Mapping[str, str] | None = None,
) -> tuple[str, ...]:
    discovered = discover_public_checker_source_paths(repo_root)
    discovered_names = {path.as_posix() for path in discovered}
    overrides = dict(source_overrides or {})
    unexpected = set(overrides) - discovered_names
    if unexpected:
        fail(f"public checker source override is outside discovery: {sorted(unexpected)}")
    for relative in discovered:
        name = relative.as_posix()
        try:
            text = overrides.get(name, (repo_root / relative).read_text(encoding="utf-8"))
        except (OSError, UnicodeError) as exc:
            fail(f"cannot read discovered public checker source {name}: {exc}")
        violation = public_boundary_violation(text)
        if violation is not None:
            fail(f"{name} crosses the public source boundary: {violation}")
    return tuple(path.as_posix() for path in discovered)


def expect_dict(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{label} must be an object")
    return value


def expect_list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        fail(f"{label} must be an array")
    return value


def require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        fail(f"{label} must be a nonempty string")
    if contract_path_violation(value) is not None:
        fail(f"{label} contains a private or transient path marker")
    return value


def strings(value: Any) -> Iterable[str]:
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for key, child in value.items():
            yield key
            yield from strings(child)
    elif isinstance(value, list):
        for child in value:
            yield from strings(child)


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        return expect_dict(json.loads(path.read_text(encoding="utf-8")), label)
    except (OSError, json.JSONDecodeError) as exc:
        fail(f"cannot read {label}: {exc}")


def load_contract() -> tuple[dict[str, Any], dict[str, Any], dict[str, str]]:
    docs = {
        path: (REPO_ROOT / path).read_text(encoding="utf-8") for path in PUBLIC_DOC_PATHS
    }
    return (
        load_json(SUPPORT_MATRIX_PATH, "provider support matrix"),
        load_json(CAPABILITY_PATH, "MCP attribution capabilities"),
        docs,
    )


def canonical_bound(bound: dict[str, Any]) -> str:
    return json.dumps(bound, sort_keys=True, separators=(",", ":"))


def version_key(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in version.split("-", 1)[0].split("."))


def validate_unique_strings(value: Any, label: str, pattern: re.Pattern[str]) -> list[str]:
    items = expect_list(value, label)
    for index, item in enumerate(items):
        text = require_string(item, f"{label}[{index}]")
        if not pattern.fullmatch(text):
            fail(f"{label}[{index}] is outside the closed grammar: {text!r}")
    if len(items) != len(set(items)):
        fail(f"{label} contains duplicates")
    return items


def validate_producer_bound(value: Any, label: str, status: str) -> dict[str, Any]:
    bound = expect_dict(value, label)
    kind = bound.get("kind")
    expected_unknown = "excluded" if status == "excluded" else "not-qualified"
    if bound.get("unknown_generations") != expected_unknown:
        fail(f"{label}.unknown_generations must be {expected_unknown!r}")

    if kind == "unversioned":
        if set(bound) != {"kind", "generation", "unknown_generations"}:
            fail(f"{label} unversioned bound has opaque fields")
        generation = bound.get("generation")
        if isinstance(generation, bool) or not isinstance(generation, int) or generation < 1:
            fail(f"{label}.generation must be a positive integer")
        if status == "excluded":
            fail(f"{label} cannot use an unversioned bound for excluded evidence")
    elif kind == "source_commits":
        if set(bound) != {"kind", "commits", "unknown_generations"}:
            fail(f"{label} source_commits bound has opaque fields")
        commits = validate_unique_strings(bound.get("commits"), f"{label}.commits", SHA_RE)
        if not commits or commits != sorted(commits):
            fail(f"{label}.commits must be a nonempty sorted list")
        if status == "excluded":
            fail(f"{label} cannot use source commits for excluded evidence")
    elif kind == "versions":
        if set(bound) != {
            "kind",
            "versions",
            "ranges",
            "source_commits",
            "unknown_generations",
        }:
            fail(f"{label} versions bound has opaque fields")
        versions = validate_unique_strings(
            bound.get("versions"), f"{label}.versions", VERSION_RE
        )
        if versions != sorted(versions, key=version_key):
            fail(f"{label}.versions must use ascending version order")
        commits = validate_unique_strings(
            bound.get("source_commits"), f"{label}.source_commits", SHA_RE
        )
        if commits != sorted(commits):
            fail(f"{label}.source_commits must be sorted")
        ranges = expect_list(bound.get("ranges"), f"{label}.ranges")
        parsed_ranges: list[tuple[tuple[int, ...], tuple[int, ...]]] = []
        for index, raw_range in enumerate(ranges):
            version_range = expect_dict(raw_range, f"{label}.ranges[{index}]")
            if set(version_range) != {"minimum", "maximum"}:
                fail(f"{label}.ranges[{index}] must define minimum and maximum")
            minimum = require_string(version_range.get("minimum"), "range minimum")
            maximum = require_string(version_range.get("maximum"), "range maximum")
            if not VERSION_RE.fullmatch(minimum) or not VERSION_RE.fullmatch(maximum):
                fail(f"{label}.ranges[{index}] is outside the version grammar")
            low, high = version_key(minimum), version_key(maximum)
            if low > high:
                fail(f"{label}.ranges[{index}] is descending")
            parsed_ranges.append((low, high))
        if parsed_ranges != sorted(parsed_ranges):
            fail(f"{label}.ranges must use ascending order")
        if any(left[1] >= right[0] for left, right in zip(parsed_ranges, parsed_ranges[1:])):
            fail(f"{label}.ranges overlap")
        if not versions and not ranges:
            fail(f"{label} must declare at least one version or range")
        if status == "exact" and not commits:
            fail(f"{label} exact version evidence must include source_commits")
        if status == "excluded":
            fail(f"{label} cannot use versions for excluded evidence")
    elif kind == "hosted_boundary":
        if set(bound) != {"kind", "unknown_generations"}:
            fail(f"{label} hosted_boundary bound has opaque fields")
        if status != "excluded":
            fail(f"{label} hosted_boundary is reserved for excluded evidence")
    else:
        fail(f"{label}.kind is outside the closed grammar: {kind!r}")
    return bound


def bound_commits(bound: dict[str, Any]) -> set[str]:
    return set(bound.get("commits", bound.get("source_commits", [])))


def bounds_overlap(left: dict[str, Any], right: dict[str, Any]) -> bool:
    if left["kind"] == right["kind"] == "unversioned":
        return left["generation"] == right["generation"]
    if "unversioned" in {left["kind"], right["kind"]}:
        return False
    if left["kind"] != "versions" or right["kind"] != "versions":
        return bool(bound_commits(left) & bound_commits(right))

    def intervals(bound: dict[str, Any]) -> list[tuple[tuple[int, ...], tuple[int, ...]]]:
        result = [(version_key(item), version_key(item)) for item in bound["versions"]]
        result.extend(
            (version_key(item["minimum"]), version_key(item["maximum"]))
            for item in bound["ranges"]
        )
        return result

    return any(
        low_a <= high_b and low_b <= high_a
        for low_a, high_a in intervals(left)
        for low_b, high_b in intervals(right)
    )


def expected_base_routes(
    support_by_id: dict[str, dict[str, Any]],
) -> set[tuple[str, str, str]]:
    routes: set[tuple[str, str, str]] = set()
    for provider_id, row in support_by_id.items():
        if row.get("status") != "supported":
            fail(f"general support changed for {provider_id}")
        for index, raw_path in enumerate(expect_list(row.get("implemented_paths"), provider_id)):
            path = expect_dict(raw_path, f"{provider_id}.implemented_paths[{index}]")
            source_format = require_string(path.get("source_format"), "source_format")
            routes.add((provider_id, "native_import", source_format))
    routes.add(("deepagents", "hosted_trace", "langsmith_trace"))
    return routes


def codex_version_bound(version: str) -> dict[str, Any]:
    return {
        "kind": "versions",
        "versions": [version],
        "ranges": [],
        "source_commits": [CODEX_PUBLIC_SOURCE_COMMIT],
        "unknown_generations": "not-qualified",
    }


def validate_codex_partition(
    rows: dict[tuple[str, str, str, str, str], dict[str, Any]],
) -> None:
    session_rows = [row for key, row in rows.items() if key[:3] == CODEX_SESSION_ROUTE]
    history_rows = [row for key, row in rows.items() if key[:3] == CODEX_HISTORY_ROUTE]
    exact_rows = [row for row in session_rows if row["status"] == "exact"]
    if len(exact_rows) != 1:
        fail("Codex must have exactly one exact session-tree partition")
    exact = exact_rows[0]
    if exact["source_schema"] != CODEX_EXACT_SCHEMA or exact["producer_bound"] != CODEX_EXACT_BOUND:
        fail("Codex exact partition must be unversioned generation 1 only")

    expected_versions = {
        canonical_bound(codex_version_bound(version)) for version in CODEX_NOT_QUALIFIED_VERSIONS
    }
    actual_versions = {
        canonical_bound(row["producer_bound"])
        for row in session_rows
        if row["status"] == "not-qualified"
        and row.get("reason") == "writer_version_unproven"
    }
    if actual_versions != expected_versions or len(session_rows) != 4:
        fail("Codex semver partitions must remain exactly the three not-qualified lanes")
    if (
        len(history_rows) != 1
        or history_rows[0]["status"] != "not-qualified"
        or history_rows[0].get("reason") != "route_mismatch"
        or history_rows[0]["producer_bound"] != CODEX_EXACT_BOUND
    ):
        fail("Codex history must remain an unversioned generation-1 not-qualified lane")


def validate_routes(
    capability: dict[str, Any], support_by_id: dict[str, dict[str, Any]]
) -> tuple[dict[tuple[str, str, str, str, str], dict[str, Any]], Counter[str], Counter[str]]:
    expected_bases = expected_base_routes(support_by_id)
    rows: dict[tuple[str, str, str, str, str], dict[str, Any]] = {}
    by_schema: dict[tuple[str, str, str, str], list[dict[str, Any]]] = defaultdict(list)
    lane_statuses: Counter[str] = Counter()
    for index, raw_row in enumerate(expect_list(capability.get("routes"), "routes")):
        row = expect_dict(raw_row, f"routes[{index}]")
        status = require_string(row.get("status"), f"routes[{index}].status")
        if status not in ALLOWED_STATUSES:
            fail(f"routes[{index}].status has unsupported value: {status}")
        expected_fields = ROW_FIELDS | ({"reason"} if status != "exact" else set())
        if set(row) != expected_fields:
            fail(f"routes[{index}] has unexpected or missing fields")
        base = tuple(
            require_string(row.get(field), f"routes[{index}].{field}")
            for field in ("provider_id", "route", "source_format")
        )
        schema = require_string(row.get("source_schema"), f"routes[{index}].source_schema")
        bound = validate_producer_bound(
            row.get("producer_bound"), f"routes[{index}].producer_bound", status
        )
        key = (*base, schema, canonical_bound(bound))
        if key in rows:
            fail(f"duplicate full capability tuple: {key}")
        rows[key] = row
        for previous in by_schema[(*base, schema)]:
            if bounds_overlap(bound, previous["producer_bound"]):
                fail(f"overlapping producer lanes for {(*base, schema)}")
        by_schema[(*base, schema)].append(row)
        lane_statuses[status] += 1
        require_string(row.get("detail"), f"routes[{index}].detail")
        evidence = expect_list(row.get("evidence"), f"routes[{index}].evidence")
        if not evidence:
            fail(f"routes[{index}].evidence must not be empty")
        for evidence_index, raw_url in enumerate(evidence):
            url = require_string(raw_url, f"routes[{index}].evidence[{evidence_index}]")
            if not url.startswith("https://"):
                fail(f"routes[{index}].evidence[{evidence_index}] must use HTTPS")
        provider_id = base[0]
        if status == "exact" and provider_id not in EXACT_PROVIDERS:
            fail(f"unexpected exact provider: {provider_id}")
        if status == "not-qualified":
            expected_reason = (
                "writer_version_unproven"
                if base == CODEX_SESSION_ROUTE
                else EXPECTED_NOT_QUALIFIED_REASONS.get(provider_id)
            )
            if row.get("reason") != expected_reason:
                fail(f"not-qualified reason mismatch for {provider_id}")
        if status == "excluded" and (
            base != ("deepagents", "hosted_trace", "langsmith_trace")
            or row.get("reason") != "outside_local_only_boundary"
        ):
            fail(f"unexpected excluded lane: {key}")

    actual_bases = {key[:3] for key in rows}
    if actual_bases != expected_bases:
        fail(
            "base route partition mismatch; "
            f"missing={sorted(expected_bases - actual_bases)}, "
            f"extra={sorted(actual_bases - expected_bases)}"
        )
    validate_codex_partition(rows)
    provider_statuses: Counter[str] = Counter()
    for provider_id in support_by_id:
        statuses = {row["status"] for key, row in rows.items() if key[0] == provider_id}
        if "exact" in statuses:
            provider_status = "exact"
        elif statuses == {"excluded"}:
            provider_status = "excluded"
        else:
            provider_status = "not-qualified"
        provider_statuses[provider_status] += 1
    provider_statuses = Counter(
        {status: provider_statuses[status] for status in EXPECTED_PROVIDER_STATUS_COUNTS}
    )
    if dict(provider_statuses) != EXPECTED_PROVIDER_STATUS_COUNTS:
        fail(f"provider status arithmetic mismatch: {dict(provider_statuses)}")
    exact = {key[0] for key, row in rows.items() if row["status"] == "exact"}
    if exact != EXACT_PROVIDERS:
        fail(f"exact provider partition mismatch: {sorted(exact)}")
    expected_counts = expect_dict(capability.get("expected_counts"), "expected_counts")
    actual_counts = {
        "providers": len(support_by_id),
        "base_routes": len(actual_bases),
        "capability_lanes": len(rows),
        "lane_statuses": dict(lane_statuses),
        "provider_statuses": dict(provider_statuses),
    }
    if actual_counts != EXPECTED_COUNTS or expected_counts != EXPECTED_COUNTS:
        fail(f"expected_counts does not match exact arithmetic: {actual_counts}")
    return rows, lane_statuses, provider_statuses


def exact_bound_urls(row: dict[str, Any], label: str) -> list[str]:
    commits = bound_commits(row["producer_bound"])
    urls = expect_list(row.get("evidence"), f"{label}.evidence")
    refs: set[str] = set()
    for index, url in enumerate(urls):
        match = PINNED_GITHUB_RE.fullmatch(url)
        if match is None:
            fail(f"{label}.evidence[{index}] is not a pinned GitHub blob/tree URL")
        refs.add(match.group(1))
    if commits and refs != commits:
        fail(f"{label} evidence refs do not equal producer source commits")
    if not refs:
        fail(f"{label} exact evidence has no pinned public source reference")
    return urls


def validate_evidence_url_reachable(
    url: str, opener: Callable[..., Any] = urlopen
) -> None:
    def request(method: str) -> None:
        headers = {"Range": "bytes=0-0"} if method == "GET" else {}
        with opener(Request(url, method=method, headers=headers), timeout=15) as response:
            status = getattr(response, "status", response.getcode())
            if not 200 <= status < 300:
                fail(f"evidence URL returned HTTP {status}: {url}")

    try:
        request("HEAD")
    except HTTPError as exc:
        if exc.code not in {405, 501}:
            fail(f"evidence URL HEAD returned HTTP {exc.code}: {url}")
        try:
            request("GET")
        except (HTTPError, URLError, OSError) as fallback_exc:
            fail(f"evidence URL fallback GET failed for {url}: {fallback_exc}")
    except (URLError, OSError) as exc:
        fail(f"evidence URL HEAD failed for {url}: {exc}")


def relative_file(path_text: Any, label: str) -> Path:
    text = require_string(path_text, label)
    path = Path(text)
    if path.is_absolute() or ".." in path.parts:
        fail(f"{label} must be a repository-relative path")
    resolved = (REPO_ROOT / path).resolve()
    if not resolved.is_relative_to(REPO_ROOT.resolve()) or not resolved.is_file():
        fail(f"{label} does not resolve to a repository file: {text}")
    return resolved


def target_block(build_text: str, target_name: str) -> str:
    offset = 0
    while True:
        start = build_text.find("ctx_rust_test(", offset)
        if start < 0:
            fail(f"Bazel target {target_name!r} is not a ctx_rust_test")
        depth, quote, escaped = 0, None, False
        for index in range(start + len("ctx_rust_test"), len(build_text)):
            character = build_text[index]
            if quote:
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == quote:
                    quote = None
                continue
            if character in {'"', "'"}:
                quote = character
            elif character == "(":
                depth += 1
            elif character == ")":
                depth -= 1
                if depth == 0:
                    block = build_text[start : index + 1]
                    if re.search(rf'\bname\s*=\s*"{re.escape(target_name)}"', block):
                        return block
                    offset = index + 1
                    break


def resolve_suite(suite_id: str) -> tuple[str, Path, str, Path]:
    if "::" not in suite_id:
        fail(f"suite_id is not package::Cargo-target-binding: {suite_id!r}")
    package, binding = suite_id.split("::", 1)
    workspace = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    matches: list[tuple[Path, dict[str, Any]]] = []
    for member in expect_list(
        expect_dict(workspace.get("workspace"), "Cargo workspace").get("members"),
        "Cargo workspace members",
    ):
        member_text = require_string(member, "Cargo workspace member")
        relative = Path(member_text)
        if relative.is_absolute() or ".." in relative.parts:
            fail(f"Cargo workspace member must be a repository-relative path: {member_text}")
        manifest = REPO_ROOT / relative / "Cargo.toml"
        if not manifest.is_file():
            fail(f"Cargo workspace member manifest is missing: {member_text}")
        package_data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        cargo_package = expect_dict(package_data.get("package"), f"{relative} package")
        if cargo_package.get("name") == package:
            matches.append((relative, package_data))
    if len(matches) != 1:
        fail(f"suite_id package does not resolve through live Cargo membership: {suite_id}")
    package_relative, _package_data = matches[0]
    package_dir = REPO_ROOT / package_relative
    if binding == "native_unit":
        target_name = "unit_tests"
    elif binding.startswith("test:") and len(binding) > len("test:"):
        target_name = f"{binding.removeprefix('test:')}_tests"
    else:
        fail(f"suite_id binding is not a supported Cargo test binding: {suite_id}")
    try:
        block = target_block(
            (package_dir / "BUILD.bazel").read_text(encoding="utf-8"), target_name
        )
    except CapabilityError:
        fail(f"suite_id does not resolve through live Cargo/Bazel ownership: {suite_id}")
    manifest = package_dir / "Cargo.toml"
    return binding, package_dir, block, manifest


def source_module_prefix(source: Path, package_dir: Path) -> tuple[str, ...]:
    source_root = package_dir / "src"
    try:
        relative = source.relative_to(source_root)
    except ValueError:
        return ()
    normal = list(relative.with_suffix("").parts)
    if normal[-1] in {"lib", "main", "mod"}:
        normal.pop()
    for parent in source_root.rglob("*.rs"):
        text = parent.read_text(encoding="utf-8")
        pattern = re.compile(
            r'#\[path\s*=\s*"([^"]+)"\]\s*(?:#\[[^\]]+\]\s*)*mod\s+(\w+)\s*;'
        )
        for path_text, module in pattern.findall(text):
            if (parent.parent / path_text).resolve() == source.resolve():
                parent_prefix = list(parent.relative_to(source_root).with_suffix("").parts)
                if parent_prefix[-1] in {"lib", "main", "mod"}:
                    parent_prefix.pop()
                return tuple(parent_prefix + [module])
    return tuple(normal)


def validate_test_source(
    test: dict[str, Any], label: str, binding: str, package_dir: Path, target: str
) -> str:
    if set(test) != {"id", "source"}:
        fail(f"{label} must define only id and source")
    test_id = require_string(test.get("id"), f"{label}.id")
    source = relative_file(test.get("source"), f"{label}.source")
    text = source.read_text(encoding="utf-8")
    function = test_id.rsplit("::", 1)[-1]
    match = re.search(
        rf'(?m)(?P<attrs>(?:^[ \t]*#\[[^\n]+\][ \t]*\n)+)'
        rf'^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?fn[ \t]+{re.escape(function)}[ \t]*\(',
        text,
    )
    if match is None or "#[test]" not in match.group("attrs"):
        fail(f"{label}.id does not name a #[test] function in {test['source']}")
    if binding == "native_unit":
        try:
            source.relative_to(package_dir / "src")
        except ValueError:
            fail(f"{label}.source is outside the native unit-test source inventory")
        if "RUST_SRCS" not in target:
            fail("native unit target no longer owns the Rust source inventory")
        prefix = source_module_prefix(source, package_dir)
        modules = tuple(test_id.split("::")[:-1])
        if modules[: len(prefix)] != prefix:
            fail(f"{label}.id module path does not resolve to {test['source']}")
        for inline_module in modules[len(prefix) :]:
            if not re.search(rf'\bmod\s+{re.escape(inline_module)}\s*\{{', text[: match.start()]):
                fail(f"{label}.id names nonexistent module {inline_module!r}")
    else:
        source_rel = source.relative_to(package_dir).as_posix()
        if f'"{source_rel}"' not in target:
            fail(f"{label}.source is absent from the resolved suite target")
        if "::" in test_id:
            fail(f"{label}.id has a module path absent from this integration suite")
    return test_id


def validate_exact_checks(
    capability: dict[str, Any],
    rows: dict[tuple[str, str, str, str, str], dict[str, Any]],
    link_checker: Callable[[str], None],
) -> tuple[int, int, int]:
    checked: set[tuple[str, str, str, str, str]] = set()
    all_test_ids: set[tuple[str, str]] = set()
    suites: set[str] = set()
    link_count = 0
    for index, raw_check in enumerate(expect_list(capability.get("exact_checks"), "exact_checks")):
        label = f"exact_checks[{index}]"
        check = expect_dict(raw_check, label)
        if set(check) != CHECK_FIELDS:
            fail(f"{label} has unexpected or missing fields")
        base = tuple(
            require_string(check.get(field), f"{label}.{field}")
            for field in ("provider_id", "route", "source_format")
        )
        schema = require_string(check.get("source_schema"), f"{label}.source_schema")
        bound = validate_producer_bound(
            check.get("producer_bound"), f"{label}.producer_bound", "exact"
        )
        key = (*base, schema, canonical_bound(bound))
        if key in checked:
            fail(f"duplicate exact check for full tuple: {key}")
        checked.add(key)
        row = rows.get(key)
        if row is None or row["status"] != "exact":
            fail(f"{label} does not match an exact full capability tuple")
        if check["producer_bound"] != row["producer_bound"]:
            fail(f"{label}.producer_bound is not pinned to its exact row")
        implementation = relative_file(
            check.get("implementation_source"), f"{label}.implementation_source"
        )
        implementation_text = implementation.read_text(encoding="utf-8")
        if f'"{schema}"' not in implementation_text:
            fail(f"{label}.source_schema is absent from its implementation source")
        suite_id = require_string(check.get("suite_id"), f"{label}.suite_id")
        binding, package_dir, target, _manifest = resolve_suite(suite_id)
        suites.add(suite_id)
        tests = expect_list(check.get("tests"), f"{label}.tests")
        minimum_test_count = 2 if base[0] == "codex" else 3
        if len(tests) < minimum_test_count:
            fail(
                f"{label} must name at least {minimum_test_count} authoritative exact tests"
            )
        for test_index, raw_test in enumerate(tests):
            test = expect_dict(raw_test, f"{label}.tests[{test_index}]")
            test_id = validate_test_source(
                test, f"{label}.tests[{test_index}]", binding, package_dir, target
            )
            test_key = (suite_id, test_id)
            if test_key in all_test_ids:
                fail(f"duplicate executable suite/test claim: {test_key}")
            all_test_ids.add(test_key)
        for url in exact_bound_urls(row, label):
            link_checker(url)
            link_count += 1

    exact_rows = {key for key, row in rows.items() if row["status"] == "exact"}
    if checked != exact_rows:
        fail("exact check inventory does not cover every exact full capability tuple")
    return len(suites), len(all_test_ids), link_count


def validate_public_docs(
    docs: dict[str, str], checker_source_overrides: Mapping[str, str] | None = None
) -> None:
    for path in PUBLIC_DOC_PATHS:
        if path not in docs:
            fail(f"public docs input is missing {path}")
        violation = public_boundary_violation(docs[path])
        if violation is not None:
            fail(f"{path} crosses the public documentation boundary: {violation}")
        for removed in ("mcp_tool_call", "mcpToolCall", "mcp_exchange", "mcpExchange"):
            if removed in docs[path]:
                fail(f"{path} still claims removed Core attribution field {removed}")
        normalized_doc = " ".join(docs[path].split())
        for stale_search_claim in (
            "`activity` is not indexed",
            "Activity adds no MCP selector, query input, tool, SQL surface, or search behavior",
            "It is not added to MCP or CLI search inputs, matching, ranking, snippets",
            "Activity is retrievable from full-content event output. It is not copied into lexical terms, semantic text",
            "result status/timing, structured result, and literal facts are not copied into search terms",
        ):
            if stale_search_claim in normalized_doc:
                fail(f"{path} contradicts the Core activity search projection")
    validate_public_checker_sources(source_overrides=checker_source_overrides)
    fixed = "Capability revision 4 exact providers are Codex, Warp, and Copilot CLI."
    for path in ("docs/provider-support.md", "docs/providers.md"):
        normalized_provider_doc = " ".join(docs[path].split())
        if fixed not in normalized_provider_doc:
            fail(f"{path} contradicts or omits the three-provider revision-4 contract")
        if "capability revision 1" in docs[path].lower():
            fail(f"{path} still claims capability revision 1")
    main = docs["docs/mcp-tool-call-attribution.md"]
    normalized_main = " ".join(main.split())
    required = (
        "Codex `codex_session_jsonl_tree` / `codex-nativepath-jsonl-v0`",
        "52 capability lanes: three `exact`, 48 `not-qualified`, and one `excluded`",
        "`activity.invocation`",
        "`provider_call_id`",
        "`protocol` equal to `mcp`",
        "an empty provider result string is `absent` in the text channel and remains an exact empty string in the structured-content channel",
        "`--content text` and `--content none` omit `activity`",
        "versions 0.200.0, 0.201.0, and 0.202.0 are separate explicit `not-qualified` lanes",
        "Warp `warp_sqlite` / `warp-agent-task-protobuf-v1`",
        "Copilot CLI `copilot_cli_session_events_jsonl` / `copilot-cli-direct-native-jsonl-v1`",
        "Machine JSON/JSONL and MCP `structuredContent` preserve the admitted activity value exactly",
        "There is no dedicated server/tool filter, query selector, SQL column, or separate MCP attribution command",
        "retained invocation protocol/server/tool/present arguments, result status/present text/structured content, and literal facts enter the shared Core search projection",
        "supplies lexical terms and snippets as well as semantic source text",
        "content-governed Core activity, not a dedicated top-level attribution object",
        "--content full",
        "--mode log",
        "client-side",
        "Deep Agents contributes its local SQLite import plus a separately excluded hosted trace",
        "private local data",
        "Reported by [@j2h4u]",
    )
    for snippet in required:
        if snippet not in normalized_main:
            fail(f"public attribution docs are missing exact contract text: {snippet}")
    if "Co-authored-by" in main:
        fail("reporter credit must not be represented as authorship")


def validate_contract(
    support: dict[str, Any],
    capability: dict[str, Any],
    docs: dict[str, str],
    link_checker: Callable[[str], None] = validate_evidence_url_reachable,
    checker_source_overrides: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    if support.get("schema_version") != 2:
        fail("provider support matrix schema_version must be 2")
    if capability.get("schema_version") != 5 or capability.get("capability_revision") != 4:
        fail("capability schema_version must be 5 and capability_revision must be 4")
    if capability.get("capability") != "exact_mcp_tool_call_attribution":
        fail("unexpected capability name")
    if capability.get("key_fields") != [
        "provider_id",
        "route",
        "source_format",
        "source_schema",
        "producer_bound",
    ]:
        fail("key_fields must freeze the full capability tuple ordering")
    if capability.get("producer_bound_grammar") != PRODUCER_BOUND_GRAMMAR:
        fail("producer_bound_grammar does not match the checker\'s closed grammar")
    definitions = expect_dict(capability.get("status_definitions"), "status_definitions")
    if set(definitions) != ALLOWED_STATUSES:
        fail("status_definitions must define exact, not-qualified, and excluded")
    for status, description in definitions.items():
        require_string(description, f"status_definitions.{status}")
    failure_definitions = expect_dict(
        capability.get("failure_reason_definitions"), "failure_reason_definitions"
    )
    if set(failure_definitions) != set(EXPECTED_NOT_QUALIFIED_REASONS.values()):
        fail("failure_reason_definitions do not match the typed FAIL taxonomy")
    for reason, description in failure_definitions.items():
        require_string(description, f"failure_reason_definitions.{reason}")
    exclusion_definitions = expect_dict(
        capability.get("exclusion_reason_definitions"), "exclusion_reason_definitions"
    )
    if set(exclusion_definitions) != {"outside_local_only_boundary"}:
        fail("exclusion_reason_definitions must define the hosted boundary")
    require_string(
        exclusion_definitions["outside_local_only_boundary"],
        "exclusion_reason_definitions.outside_local_only_boundary",
    )
    for value in strings(capability):
        if contract_path_violation(value) is not None:
            fail("capability contract contains private or transient path wording")
        if "not_assessed" in value:
            fail("capability contract contains a not_assessed state")

    support_by_id: dict[str, dict[str, Any]] = {}
    for index, raw_row in enumerate(expect_list(support.get("providers"), "support providers")):
        row = expect_dict(raw_row, f"support providers[{index}]")
        provider_id = require_string(row.get("id"), f"support providers[{index}].id")
        if provider_id in support_by_id:
            fail(f"duplicate support provider: {provider_id}")
        support_by_id[provider_id] = row
    if len(support_by_id) != 43:
        fail(f"support matrix must contain 43 providers, found {len(support_by_id)}")

    rows, lane_statuses, provider_statuses = validate_routes(capability, support_by_id)
    exact_suites, exact_tests, exact_links = validate_exact_checks(
        capability, rows, link_checker
    )
    validate_public_docs(docs, checker_source_overrides)
    return {
        "providers": len(support_by_id),
        "base_routes": len({key[:3] for key in rows}),
        "capability_lanes": len(rows),
        "lane_statuses": dict(lane_statuses),
        "provider_statuses": dict(provider_statuses),
        "exact_suites": exact_suites,
        "exact_tests": exact_tests,
        "exact_links": exact_links,
    }

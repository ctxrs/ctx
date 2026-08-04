#!/usr/bin/env python3

from __future__ import annotations

import copy
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

from mcp_attribution_contract_registry import (
    FIXTURE_ROUTE_SCHEMA_CONTRACT,
    ROUTE_CONTRACT_CLASSIFICATION,
    ROUTE_SCHEMA_CONTRACTS,
    RouteSchemaContract,
)
import mcp_attribution_conformance as conformance
from mcp_attribution_conformance import (
    CAPABILITY_REVISION,
    ConformanceError,
    MANIFEST_SCHEMA_VERSION,
    PUBLIC_VALIDATION_MODE,
    REQUIRED_EVIDENCE_CLASSES,
    SuiteBinding,
    _merge_suite_bindings,
    _parse_suite_bindings,
    run_conformance as run_conformance_impl,
    validate_manifest as validate_manifest_impl,
)


PI_ROUTE_SCHEMA_CONTRACT = next(
    contract for contract in ROUTE_SCHEMA_CONTRACTS if contract.provider == "pi"
)
DEEPAGENTS_ROUTE_SCHEMA_CONTRACT = next(
    contract for contract in ROUTE_SCHEMA_CONTRACTS if contract.provider == "deepagents"
)


def repository_root() -> Path:
    marker = Path("crates/ctx-history-capture/tests/mcp_attribution_conformance.py")
    for candidate in [
        Path(__file__).resolve().parents[3],
        Path.cwd().resolve(),
        Path(__file__).absolute().parents[3],
    ]:
        if (candidate / marker).is_file():
            return candidate
    raise RuntimeError("cannot locate repository root for test shape contract")


def structural_schema(revision: int = 1) -> dict:
    return {
        "kind": "structural_revision",
        "revision": revision,
        "shape_sha256": FIXTURE_ROUTE_SCHEMA_CONTRACT.sha256,
    }


def route_contract_provenance(
    contract: RouteSchemaContract = FIXTURE_ROUTE_SCHEMA_CONTRACT,
) -> dict:
    return {
        "kind": "route_schema_contract",
        "classification": contract.classification,
        "reference": contract.path,
        "sha256": contract.sha256,
    }


def contract_format_schema(contract: RouteSchemaContract) -> dict:
    return {**contract.format_schema, "shape_sha256": contract.sha256}


def tracked_route_contract(
    root: Path,
    *,
    revision: int = 1,
    producer_domain: dict | None = None,
    filename: str = "shape-contract.json",
) -> RouteSchemaContract:
    format_schema = {"kind": "structural_revision", "revision": revision}
    domain = producer_domain or discrete(unversioned())
    relative = Path(
        "crates/ctx-history-capture/tests/contracts/mcp-attribution-fixtures/"
        f"fixture/fixture_jsonl/{filename}"
    )
    document = {
        "contract_schema_version": 1,
        "classification": ROUTE_CONTRACT_CLASSIFICATION,
        "provider": "fixture",
        "route": "native_import",
        "source_format": "fixture_jsonl",
        "format_schema": format_schema,
        "producer_domain": domain,
    }
    artifact = root / relative
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(
        ["git", "-C", str(root), "add", "--", relative.as_posix()], check=True
    )
    return RouteSchemaContract(
        provider="fixture",
        route="native_import",
        source_format="fixture_jsonl",
        format_schema=format_schema,
        producer_domain=domain,
        path=relative.as_posix(),
        allowed_subtree=relative.parent.as_posix(),
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256=hashlib.sha256(artifact.read_bytes()).hexdigest(),
    )


def bind_manifest_contract(manifest: dict, contract: RouteSchemaContract) -> None:
    schema = manifest["schema_generations"][0]
    schema["format_schema"] = contract_format_schema(contract)
    schema["producer_domain"] = copy.deepcopy(contract.producer_domain)
    schema["provenance"] = route_contract_provenance(contract)
    for lane in manifest["capability_lanes"]:
        lane["format_schema"] = contract_format_schema(contract)


def minimal_matrix(provider: str = "fixture", source_format: str = "fixture_jsonl") -> dict:
    return {
        "schema_version": 1,
        "scope": "fixture",
        "providers": [
            {
                "id": provider,
                "status": "supported",
                "implemented_paths": [
                    {"kind": "native_import", "source_format": source_format}
                ],
            }
        ],
    }


def unversioned(generation: int = 1) -> dict:
    return {"kind": "unversioned_generation", "generation": generation}


def discrete(*versions: dict) -> dict:
    return {"kind": "discrete", "versions": list(versions)}


def interval(lower: str, upper: str) -> dict:
    return {
        "kind": "semver_interval",
        "lower_inclusive": lower,
        "upper_exclusive": upper,
    }


def fixture_producer_bound(partition: dict) -> dict:
    if partition["kind"] == "semver_interval":
        return {
            "kind": "versions",
            "versions": [],
            "ranges": [
                {
                    "minimum": partition["lower_inclusive"],
                    "maximum": partition["upper_exclusive"],
                }
            ],
            "source_commits": [],
            "unknown_generations": "not-qualified",
        }
    point = partition["versions"][0]
    if point["kind"] == "unversioned_generation":
        return {
            "kind": "unversioned",
            "generation": point["generation"],
            "unknown_generations": "not-qualified",
        }
    return {
        "kind": "versions",
        "versions": [point["version"]],
        "ranges": [],
        "source_commits": [],
        "unknown_generations": "not-qualified",
    }


def sync_fixture_producer_bounds(manifest: dict) -> None:
    for lane in manifest["capability_lanes"]:
        bound = fixture_producer_bound(lane["producer_partition"])
        if lane["status"]["kind"] == "excluded":
            bound["unknown_generations"] = "excluded"
        lane["producer_bound"] = bound
        schema = lane["format_schema"]
        if schema.get("kind") == "structural_revision":
            lane["source_schema"] = f"fixture-jsonl-v{schema['revision']}"


def capability_audit_from_manifest(manifest: dict) -> dict:
    manifest_to_audit = {
        "supported": "exact",
        "not_qualified": "not-qualified",
        "excluded": "excluded",
    }
    routes = []
    provider_statuses: dict[str, set[str]] = {}
    lane_statuses = {value: 0 for value in manifest_to_audit.values()}
    for lane in manifest["capability_lanes"]:
        status = manifest_to_audit.get(lane["status"]["kind"], "not-qualified")
        lane_statuses[status] += 1
        provider_statuses.setdefault(lane["provider"], set()).add(status)
        routes.append(
            {
                "provider_id": lane["provider"],
                "route": lane["route"],
                "source_format": lane["source_format"],
                "source_schema": lane["source_schema"],
                "producer_bound": copy.deepcopy(lane["producer_bound"]),
                "status": status,
            }
        )
    provider_rows = {value: 0 for value in manifest_to_audit.values()}
    for statuses in provider_statuses.values():
        if "exact" in statuses:
            provider_rows["exact"] += 1
        elif statuses == {"excluded"}:
            provider_rows["excluded"] += 1
        else:
            provider_rows["not-qualified"] += 1
    return {
        "expected_counts": {
            "providers": manifest["expected_provider_count"],
            "base_routes": manifest["expected_base_route_count"],
            "capability_lanes": manifest["expected_capability_lane_count"],
            "lane_statuses": lane_statuses,
            "provider_statuses": provider_rows,
        },
        "routes": routes,
    }


def minimal_manifest() -> dict:
    base = {
        "provider": "fixture",
        "route": "native_import",
        "source_format": "fixture_jsonl",
    }
    format_schema = structural_schema()
    return {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "capability": "mcp_tool_call_attribution",
        "capability_revision": CAPABILITY_REVISION,
        "expected_provider_count": 1,
        "expected_base_route_count": 1,
        "expected_schema_generation_count": 1,
        "expected_capability_lane_count": 1,
        "expected_status_rows": {
            "supported": 0,
            "not_qualified": 1,
            "excluded": 0,
        },
        "expected_provider_status_rows": {
            "supported": 0,
            "not_qualified": 1,
            "excluded": 0,
        },
        "required_supported_evidence_classes": sorted(REQUIRED_EVIDENCE_CLASSES),
        "base_routes": [base],
        "schema_generations": [
            {
                **base,
                "format_schema": copy.deepcopy(format_schema),
                "producer_domain": discrete(unversioned()),
                "provenance": route_contract_provenance(),
            }
        ],
        "capability_lanes": [
            {
                **base,
                "source_schema": "fixture-jsonl-v1",
                "format_schema": copy.deepcopy(format_schema),
                "producer_partition": discrete(unversioned()),
                "producer_bound": fixture_producer_bound(discrete(unversioned())),
                "status": {
                    "kind": "not_qualified",
                    "reason": {
                        "kind": "format_not_audited",
                        "evidence_ref": "fixture:format-audit",
                    },
                },
                "evidence": [],
            }
        ],
    }


def supported_manifest() -> tuple[dict, dict[str, set[str]]]:
    manifest = minimal_manifest()
    lane = manifest["capability_lanes"][0]
    lane["status"] = {"kind": "supported"}
    public_capabilities: dict[str, set[str]] = {}
    for evidence_class in sorted(REQUIRED_EVIDENCE_CLASSES):
        test = f"fixture_{evidence_class}"
        public_capabilities[test] = {evidence_class}
        lane["evidence"].append(
            {
                "class": evidence_class,
                "kind": "rust_test",
                "suite": "fixture_public",
                "test": test,
                "scope": "tuple",
            }
        )
    manifest["expected_status_rows"] = {
        "supported": 1,
        "not_qualified": 0,
        "excluded": 0,
    }
    manifest["expected_provider_status_rows"] = {
        "supported": 1,
        "not_qualified": 0,
        "excluded": 0,
    }
    return manifest, public_capabilities


def deepagents_manifest() -> dict:
    local_base = {
        "provider": "deepagents",
        "route": "native_import",
        "source_format": "deepagents_sessions_sqlite",
    }
    hosted_base = {
        "provider": "deepagents",
        "route": "hosted_trace",
        "source_format": "langsmith_trace",
    }
    local_schema = contract_format_schema(DEEPAGENTS_ROUTE_SCHEMA_CONTRACT)
    local_partition = copy.deepcopy(DEEPAGENTS_ROUTE_SCHEMA_CONTRACT.producer_domain)
    return {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "capability": "mcp_tool_call_attribution",
        "capability_revision": CAPABILITY_REVISION,
        "expected_provider_count": 1,
        "expected_base_route_count": 2,
        "expected_schema_generation_count": 1,
        "expected_capability_lane_count": 2,
        "expected_status_rows": {
            "supported": 0,
            "not_qualified": 1,
            "excluded": 1,
        },
        "expected_provider_status_rows": {
            "supported": 0,
            "not_qualified": 1,
            "excluded": 0,
        },
        "required_supported_evidence_classes": sorted(REQUIRED_EVIDENCE_CLASSES),
        "base_routes": [local_base, hosted_base],
        "schema_generations": [
            {
                **local_base,
                "format_schema": local_schema,
                "producer_domain": copy.deepcopy(local_partition),
                "provenance": route_contract_provenance(
                    DEEPAGENTS_ROUTE_SCHEMA_CONTRACT
                ),
            }
        ],
        "capability_lanes": [
            {
                **local_base,
                "source_schema": "deepagents-sqlite-write-messages-v0",
                "format_schema": copy.deepcopy(local_schema),
                "producer_partition": local_partition,
                "producer_bound": {
                    "kind": "unversioned",
                    "generation": 1,
                    "unknown_generations": "not-qualified",
                },
                "status": {
                    "kind": "not_qualified",
                    "reason": {
                        "kind": "identity_not_proven",
                        "evidence_ref": "capability-audit:deepagents-no-durable-server-field",
                    },
                },
                "evidence": [],
            },
            {
                **hosted_base,
                "source_schema": "hosted-trace-v1",
                "format_schema": {
                    "kind": "hosted_boundary",
                    "source_schema": "hosted-trace-v1",
                },
                "producer_partition": {"kind": "hosted_boundary"},
                "producer_bound": {
                    "kind": "hosted_boundary",
                    "unknown_generations": "excluded",
                },
                "status": {
                    "kind": "excluded",
                    "reason": {
                        "kind": "hosted_only",
                        "evidence_ref": "provider-boundary:deepagents-hosted-route",
                    },
                },
                "evidence": [],
            },
        ],
    }


def fake_test_binary(
    root: Path,
    name: str,
    mode: str,
    tests: list[str],
    *,
    target: str | None = None,
    selected_inventory: bool = False,
) -> SuiteBinding:
    binary = root / f"fake-tests-{name}-{mode}"
    binary.write_text(
        textwrap.dedent(
            f"""\
            #!/usr/bin/env python3
            import sys
            tests = {tests!r}
            if "--list" in sys.argv:
                for name in tests:
                    print(f"{{name}}: test")
                raise SystemExit(0)
            selected = tests
            if "--exact" in sys.argv:
                requested = sys.argv[1]
                selected = [requested] if requested in tests else []
            mode = {mode!r}
            if mode == "ok":
                print(
                    f"test result: ok. {{len(selected)}} passed; 0 failed; 0 ignored; "
                    f"0 measured; {{len(tests) - len(selected)}} filtered out; finished in 0.00s"
                )
                raise SystemExit(0)
            if mode == "ignored":
                print(
                    f"test result: ok. 0 passed; 0 failed; {{len(selected)}} ignored; "
                    f"0 measured; {{len(tests) - len(selected)}} filtered out; finished in 0.00s"
                )
                raise SystemExit(0)
            if mode == "zero":
                print(f"test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; {{len(tests)}} filtered out; finished in 0.00s")
                raise SystemExit(0)
            raise SystemExit(2)
            """
        ),
        encoding="utf-8",
    )
    binary.chmod(0o755)
    return SuiteBinding(
        target=target or f"//tests:{name}",
        binary=binary,
        selected_inventory=selected_inventory,
    )


def validate_fixture_manifest(
    manifest: dict,
    matrix: dict,
    audit: dict | None = None,
    *,
    contracts: tuple[RouteSchemaContract, ...] = (FIXTURE_ROUTE_SCHEMA_CONTRACT,),
    root: Path | None = None,
):
    if audit is None:
        sync_fixture_producer_bounds(manifest)
        audit = capability_audit_from_manifest(manifest)
    return validate_manifest_impl(
        manifest,
        matrix,
        root or repository_root(),
        contracts,
        audit,
    )


def run_fixture_conformance(
    manifest: dict,
    matrix: dict,
    public_binaries,
    public_capabilities,
    temp_root: Path,
    mode: str = PUBLIC_VALIDATION_MODE,
    *,
    contracts: tuple[RouteSchemaContract, ...] = (FIXTURE_ROUTE_SCHEMA_CONTRACT,),
    root: Path | None = None,
):
    sync_fixture_producer_bounds(manifest)
    return run_conformance_impl(
        manifest,
        matrix,
        public_binaries,
        public_capabilities,
        temp_root,
        mode,
        root or repository_root(),
        contracts,
    )


class ConformanceRunnerTests(unittest.TestCase):
    def test_valid_public_validation_reports_executable_counts(self) -> None:
        manifest, public_capabilities = supported_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            inventory = run_fixture_conformance(
                manifest,
                minimal_matrix(),
                {
                    "fixture_public": fake_test_binary(
                        root, "public", "ok", sorted(public_capabilities)
                    )
                },
                {"fixture_public": public_capabilities},
                root / "tmp",
            )
        self.assertEqual(inventory.provider_count, 1)
        self.assertEqual(inventory.base_route_count, 1)
        self.assertEqual(inventory.schema_generation_count, 1)
        self.assertEqual(inventory.capability_lane_count, 1)
        self.assertEqual(inventory.public_executable_count, 10)

    def test_unknown_mode_is_rejected(self) -> None:
        manifest = minimal_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            with self.assertRaisesRegex(ConformanceError, "unknown conformance mode"):
                run_fixture_conformance(
                    manifest,
                    minimal_matrix(),
                    {},
                    {},
                    Path(raw_root) / "tmp",
                    "unsupported-mode",
                )

    def test_runner_exposes_only_public_modes_and_evidence_classes(self) -> None:
        self.assertEqual(conformance.CONFORMANCE_MODES, {PUBLIC_VALIDATION_MODE})
        self.assertEqual(
            REQUIRED_EVIDENCE_CLASSES,
            {
                "ambiguity_duplicate_linkage",
                "canonical_terminal_outcomes",
                "exact_boundary",
                "exact_positive_pair",
                "malformed_identity",
                "max_plus_one",
                "privacy_sinks",
                "result_preservation",
                "search_nonindexing",
                "stable_ids",
            },
        )

    def test_unknown_manifest_extensions_are_rejected(self) -> None:
        for location, value in [
            ("manifest", []),
            ("lane", []),
        ]:
            with self.subTest(location=location):
                manifest = minimal_manifest()
                if location == "manifest":
                    manifest["nonpublic_evidence_extensions"] = value
                else:
                    manifest["capability_lanes"][0]["nonpublic_requirements"] = value
                with self.assertRaisesRegex(ConformanceError, "unknown=.*nonpublic"):
                    validate_fixture_manifest(manifest, minimal_matrix())

    def test_public_suite_registry_exports_only_public_helpers(self) -> None:
        registry = (
            repository_root()
            / "crates/ctx-history-capture/tests/mcp_attribution_suites.bzl"
        ).read_text(encoding="utf-8")
        exported_helpers = {
            line.split("(", 1)[0].removeprefix("def ")
            for line in registry.splitlines()
            if line.startswith("def mcp_attribution_")
        }
        self.assertEqual(
            exported_helpers,
            {"mcp_attribution_suite_args", "mcp_attribution_suite_data"},
        )

    def test_capability_audit_projection_rejects_lane_drift(self) -> None:
        manifest = minimal_manifest()
        sync_fixture_producer_bounds(manifest)
        audit = capability_audit_from_manifest(manifest)
        mutations = {
            "route": lambda lane: lane.update(route="other_route"),
            "schema": lambda lane: lane.update(source_schema="other-schema-v2"),
            "producer": lambda lane: lane.update(
                producer_bound={
                    "kind": "unversioned",
                    "generation": 2,
                    "unknown_generations": "not-qualified",
                }
            ),
            "status": lambda lane: lane.update(status={"kind": "supported"}),
        }
        for field, mutate in mutations.items():
            with self.subTest(field=field):
                drifted = copy.deepcopy(manifest)
                mutate(drifted["capability_lanes"][0])
                with self.assertRaisesRegex(
                    ConformanceError, "manifest capability projection differs"
                ):
                    conformance._validate_capability_audit_projection(drifted, audit)

    def test_deepagents_local_import_and_hosted_boundary_are_distinct(self) -> None:
        manifest = deepagents_manifest()
        inventory = validate_fixture_manifest(
            manifest,
            minimal_matrix("deepagents", "deepagents_sessions_sqlite"),
            capability_audit_from_manifest(manifest),
            contracts=(DEEPAGENTS_ROUTE_SCHEMA_CONTRACT,),
        )
        self.assertEqual(inventory.base_route_count, 2)
        self.assertEqual(inventory.schema_generation_count, 1)
        self.assertEqual(inventory.capability_lane_count, 2)
        self.assertEqual(
            inventory.status_rows,
            {"supported": 0, "not_qualified": 1, "excluded": 1},
        )

    def test_hosted_boundary_cannot_replace_an_imported_schema(self) -> None:
        manifest = deepagents_manifest()
        manifest["schema_generations"].append(
            {
                **manifest["base_routes"][1],
                "format_schema": {
                    "kind": "hosted_boundary",
                    "source_schema": "hosted-trace-v1",
                },
                "producer_domain": {"kind": "hosted_boundary"},
                "provenance": {
                    "kind": "capability_audit",
                    "reference": "capability-audit:deepagents-hosted-route",
                },
            }
        )
        manifest["expected_schema_generation_count"] = 2
        with self.assertRaisesRegex(
            ConformanceError, "cannot declare a schema generation for a hosted boundary"
        ):
            validate_fixture_manifest(
                manifest,
                minimal_matrix("deepagents", "deepagents_sessions_sqlite"),
                capability_audit_from_manifest(manifest),
                contracts=(DEEPAGENTS_ROUTE_SCHEMA_CONTRACT,),
            )

    def test_supported_lane_rejects_writer_version_admission_bound(self) -> None:
        manifest, _ = supported_manifest()
        sync_fixture_producer_bounds(manifest)
        manifest["capability_lanes"][0]["producer_bound"] = {
            "kind": "versions",
            "versions": ["1.2.3"],
            "ranges": [],
            "source_commits": ["a" * 40],
            "unknown_generations": "not-qualified",
        }
        audit = capability_audit_from_manifest(manifest)
        with self.assertRaisesRegex(
            ConformanceError, "structural unversioned generation 1"
        ):
            validate_fixture_manifest(manifest, minimal_matrix(), audit)

    def test_multiple_discrete_versions_of_one_schema_are_accepted(self) -> None:
        manifest = minimal_manifest()
        domain = discrete(unversioned(1), unversioned(2))
        second = copy.deepcopy(manifest["capability_lanes"][0])
        second["producer_partition"] = discrete(unversioned(2))
        manifest["capability_lanes"].append(second)
        manifest["expected_capability_lane_count"] = 2
        manifest["expected_status_rows"]["not_qualified"] = 2
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            contract = tracked_route_contract(root, producer_domain=domain)
            bind_manifest_contract(manifest, contract)
            inventory = validate_fixture_manifest(
                manifest, minimal_matrix(), contracts=(contract,), root=root
            )
        self.assertEqual(inventory.schema_generation_count, 1)
        self.assertEqual(inventory.capability_lane_count, 2)

    def test_multiple_schema_generations_of_one_base_route_are_accepted(self) -> None:
        manifest = minimal_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            first_contract = tracked_route_contract(root, filename="shape-v1.json")
            second_contract = tracked_route_contract(
                root, revision=2, filename="shape-v2.json"
            )
            bind_manifest_contract(manifest, first_contract)
            second_schema = copy.deepcopy(manifest["schema_generations"][0])
            second_schema["format_schema"] = contract_format_schema(second_contract)
            second_schema["provenance"] = route_contract_provenance(second_contract)
            manifest["schema_generations"].append(second_schema)
            second_lane = copy.deepcopy(manifest["capability_lanes"][0])
            second_lane["format_schema"] = second_schema["format_schema"]
            manifest["capability_lanes"].append(second_lane)
            manifest["expected_schema_generation_count"] = 2
            manifest["expected_capability_lane_count"] = 2
            manifest["expected_status_rows"]["not_qualified"] = 2
            inventory = validate_fixture_manifest(
                manifest,
                minimal_matrix(),
                contracts=(first_contract, second_contract),
                root=root,
            )
        self.assertEqual(inventory.schema_generation_count, 2)

    def test_complete_nonoverlapping_semver_ranges_are_accepted(self) -> None:
        manifest = minimal_manifest()
        domain = interval("1.0.0", "3.0.0")
        first = manifest["capability_lanes"][0]
        first["producer_partition"] = interval("1.0.0", "2.0.0")
        second = copy.deepcopy(first)
        second["producer_partition"] = interval("2.0.0", "3.0.0")
        manifest["capability_lanes"].append(second)
        manifest["expected_capability_lane_count"] = 2
        manifest["expected_status_rows"]["not_qualified"] = 2
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            contract = tracked_route_contract(root, producer_domain=domain)
            bind_manifest_contract(manifest, contract)
            self.assertEqual(
                validate_fixture_manifest(
                    manifest, minimal_matrix(), contracts=(contract,), root=root
                ).capability_lane_count,
                2,
            )

    def test_duplicate_full_tuple_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"].append(
            copy.deepcopy(manifest["capability_lanes"][0])
        )
        manifest["expected_capability_lane_count"] = 2
        manifest["expected_status_rows"]["not_qualified"] = 2
        with self.assertRaisesRegex(ConformanceError, "duplicate full capability tuple"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_opaque_schema_and_version_strings_are_rejected(self) -> None:
        for field, value, message in [
            (
                "format_schema",
                {"kind": "opaque", "version": "anything1"},
                "unknown schema grammar",
            ),
            (
                "producer_partition",
                discrete({"kind": "semver", "version": "banana1"}),
                "strict MAJOR.MINOR.PATCH",
            ),
        ]:
            with self.subTest(field=field):
                manifest = minimal_manifest()
                manifest["capability_lanes"][0][field] = value
                with self.assertRaisesRegex(ConformanceError, message):
                    validate_fixture_manifest(manifest, minimal_matrix())

    def test_unversioned_generation_cannot_be_a_catch_all(self) -> None:
        manifest = minimal_manifest()
        wildcard = {"kind": "unversioned_generation", "generation": "*"}
        manifest["schema_generations"][0]["producer_domain"] = discrete(wildcard)
        manifest["capability_lanes"][0]["producer_partition"] = discrete(wildcard)
        with self.assertRaisesRegex(ConformanceError, "positive integer"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_overlapping_semver_ranges_are_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["schema_generations"][0]["producer_domain"] = interval(
            "1.0.0", "3.0.0"
        )
        first = manifest["capability_lanes"][0]
        first["producer_partition"] = interval("1.0.0", "2.5.0")
        second = copy.deepcopy(first)
        second["producer_partition"] = interval("2.0.0", "3.0.0")
        manifest["capability_lanes"].append(second)
        manifest["expected_capability_lane_count"] = 2
        manifest["expected_status_rows"]["not_qualified"] = 2
        with self.assertRaisesRegex(ConformanceError, "overlapping producer ranges"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_incomplete_semver_ranges_are_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["schema_generations"][0]["producer_domain"] = interval(
            "1.0.0", "3.0.0"
        )
        first = manifest["capability_lanes"][0]
        first["producer_partition"] = interval("1.0.0", "2.0.0")
        second = copy.deepcopy(first)
        second["producer_partition"] = interval("2.1.0", "3.0.0")
        manifest["capability_lanes"].append(second)
        manifest["expected_capability_lane_count"] = 2
        manifest["expected_status_rows"]["not_qualified"] = 2
        with self.assertRaisesRegex(ConformanceError, "incomplete producer range"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_incomplete_discrete_generation_partition_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["schema_generations"][0]["producer_domain"] = discrete(
            unversioned(1), unversioned(2)
        )
        with self.assertRaisesRegex(ConformanceError, "incomplete producer partition"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_discrete_lane_cannot_bundle_multiple_producer_versions(self) -> None:
        manifest = minimal_manifest()
        versions = discrete(unversioned(1), unversioned(2))
        manifest["schema_generations"][0]["producer_domain"] = versions
        manifest["capability_lanes"][0]["producer_partition"] = versions
        with self.assertRaisesRegex(ConformanceError, "exactly one discrete producer"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_missing_and_unclaimed_base_routes_are_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["base_routes"] = []
        with self.assertRaisesRegex(ConformanceError, "base route row count"):
            validate_fixture_manifest(manifest, minimal_matrix())
        manifest = minimal_manifest()
        manifest["base_routes"][0]["source_format"] = "other_jsonl"
        with self.assertRaisesRegex(ConformanceError, "base route inventory differs"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_missing_and_unclaimed_schema_generations_are_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["schema_generations"] = []
        with self.assertRaisesRegex(ConformanceError, "schema generation count"):
            validate_fixture_manifest(manifest, minimal_matrix())
        manifest = minimal_manifest()
        manifest["capability_lanes"][0]["format_schema"]["revision"] = 2
        with self.assertRaisesRegex(ConformanceError, "unclaimed schema generation"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_coordinated_structural_revision_999_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["schema_generations"][0]["format_schema"]["revision"] = 999
        manifest["capability_lanes"][0]["format_schema"]["revision"] = 999
        with self.assertRaisesRegex(ConformanceError, "closed route schema registry"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_capability_revision_999_is_not_self_authorizing(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_revision"] = 999
        with self.assertRaisesRegex(ConformanceError, "capability_revision must equal"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_coordinated_unversioned_generation_999_is_rejected(self) -> None:
        manifest = minimal_manifest()
        generation_999 = discrete(unversioned(999))
        manifest["schema_generations"][0]["producer_domain"] = generation_999
        manifest["capability_lanes"][0]["producer_partition"] = generation_999
        with self.assertRaisesRegex(ConformanceError, "closed route schema registry"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_second_schema_cannot_reuse_a_shape_digest(self) -> None:
        manifest = minimal_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            first_contract = tracked_route_contract(root, filename="shape-v1.json")
            second_contract = tracked_route_contract(
                root, revision=2, filename="shape-v2.json"
            )
            bind_manifest_contract(manifest, first_contract)
            second_schema = copy.deepcopy(manifest["schema_generations"][0])
            second_schema["format_schema"] = {
                **second_contract.format_schema,
                "shape_sha256": first_contract.sha256,
            }
            second_schema["provenance"] = {
                **route_contract_provenance(second_contract),
                "sha256": first_contract.sha256,
            }
            manifest["schema_generations"].append(second_schema)
            second_lane = copy.deepcopy(manifest["capability_lanes"][0])
            second_lane["format_schema"] = second_schema["format_schema"]
            manifest["capability_lanes"].append(second_lane)
            manifest["expected_schema_generation_count"] = 2
            manifest["expected_capability_lane_count"] = 2
            manifest["expected_status_rows"]["not_qualified"] = 2
            with self.assertRaisesRegex(ConformanceError, "shape digest.*reused"):
                validate_fixture_manifest(
                    manifest,
                    minimal_matrix(),
                    contracts=(first_contract, second_contract),
                    root=root,
                )

    def test_bogus_manifest_shape_path_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["schema_generations"][0]["provenance"]["reference"] = (
            "crates/ctx-history-capture/tests/contracts/mcp-attribution-fixtures/"
            "fixture/fixture_jsonl/future-v999.json"
        )
        with self.assertRaisesRegex(ConformanceError, "differs from the closed"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_symlink_shape_contract_escape_is_rejected(self) -> None:
        manifest = minimal_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            contract = tracked_route_contract(root)
            artifact = root / contract.path
            escaped = root / "escaped-shape-contract.json"
            os.replace(artifact, escaped)
            artifact.symlink_to(escaped)
            bind_manifest_contract(manifest, contract)
            with self.assertRaisesRegex(ConformanceError, "contains a symlink"):
                validate_fixture_manifest(
                    manifest, minimal_matrix(), contracts=(contract,), root=root
                )

    def test_stale_shape_contract_digest_is_rejected(self) -> None:
        manifest = minimal_manifest()
        stale = replace(FIXTURE_ROUTE_SCHEMA_CONTRACT, sha256="0" * 64)
        bind_manifest_contract(manifest, stale)
        with self.assertRaisesRegex(ConformanceError, "sha256 differs"):
            validate_fixture_manifest(
                manifest, minimal_matrix(), contracts=(stale,)
            )

    def test_shape_contract_classification_is_closed(self) -> None:
        manifest = minimal_manifest()
        untrusted = replace(
            FIXTURE_ROUTE_SCHEMA_CONTRACT, classification="executable_parser_source"
        )
        bind_manifest_contract(manifest, untrusted)
        with self.assertRaisesRegex(ConformanceError, "classification is not closed"):
            validate_fixture_manifest(
                manifest, minimal_matrix(), contracts=(untrusted,)
            )

    def test_rust_file_with_function_tokens_is_not_a_shape_contract(self) -> None:
        manifest = minimal_manifest()
        reference = "crates/ctx-history-capture/src/provider/providers/pi.rs"
        parser = repository_root() / reference
        parser_contract = replace(
            FIXTURE_ROUTE_SCHEMA_CONTRACT,
            path=reference,
            allowed_subtree=str(Path(reference).parent),
            sha256=hashlib.sha256(parser.read_bytes()).hexdigest(),
        )
        bind_manifest_contract(manifest, parser_contract)
        with self.assertRaisesRegex(ConformanceError, "not valid JSON"):
            validate_fixture_manifest(
                manifest, minimal_matrix(), contracts=(parser_contract,)
            )

    def test_missing_capability_lane_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"] = []
        with self.assertRaisesRegex(ConformanceError, "capability lane inventory is empty"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_ignored_status_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"][0]["status"] = {"kind": "ignored"}
        with self.assertRaisesRegex(ConformanceError, "forbidden status"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_not_qualified_requires_typed_reason_and_provenance(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"][0]["status"] = {"kind": "not_qualified"}
        with self.assertRaisesRegex(ConformanceError, "keys differ"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_excluded_lane_without_local_boundary_reason_is_rejected(self) -> None:
        manifest = minimal_manifest()
        lane = manifest["capability_lanes"][0]
        lane["status"] = {"kind": "excluded"}
        manifest["expected_status_rows"] = {
            "supported": 0,
            "not_qualified": 0,
            "excluded": 1,
        }
        with self.assertRaisesRegex(ConformanceError, "keys differ"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_pi_cannot_overclaim_with_provider_neutral_index_test(self) -> None:
        manifest = minimal_manifest()
        for collection in ["base_routes", "schema_generations", "capability_lanes"]:
            manifest[collection][0].update(
                {
                    "provider": "pi",
                    "source_format": "pi_session_jsonl",
                }
            )
        bind_manifest_contract(manifest, PI_ROUTE_SCHEMA_CONTRACT)
        lane = manifest["capability_lanes"][0]
        lane["status"] = {"kind": "supported"}
        lane["evidence"] = [
            {
                "class": "search_nonindexing",
                "kind": "rust_test",
                "suite": "index_only",
                "test": "provider_neutral_index_canary",
                "scope": "provider_neutral",
            }
        ]
        manifest["expected_status_rows"] = {
            "supported": 1,
            "not_qualified": 0,
            "excluded": 0,
        }
        with self.assertRaisesRegex(ConformanceError, "missing required executable classes"):
            validate_fixture_manifest(
                manifest,
                minimal_matrix("pi", "pi_session_jsonl"),
                contracts=(PI_ROUTE_SCHEMA_CONTRACT,),
            )

    def test_supported_lane_missing_required_class_is_rejected(self) -> None:
        manifest, _ = supported_manifest()
        lane = manifest["capability_lanes"][0]
        lane["evidence"] = [
            evidence
            for evidence in lane["evidence"]
            if evidence["class"] != "exact_boundary"
        ]
        with self.assertRaisesRegex(ConformanceError, "exact_boundary"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_nonpublic_evidence_class_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"][0]["evidence"].append(
            {
                "class": "nonpublic_only_evidence",
                "kind": "rust_test",
                "suite": "fixture_public",
                "test": "nonpublic_claim",
                "scope": "tuple",
            }
        )
        with self.assertRaisesRegex(ConformanceError, "unknown evidence class"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_nonpublic_evidence_kind_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"][0]["evidence"].append(
            {
                "class": "search_nonindexing",
                "kind": "nonpublic_test",
                "suite": "fixture_public",
                "test": "nonpublic_claim",
                "scope": "tuple",
            }
        )
        with self.assertRaisesRegex(ConformanceError, "unknown kind"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_cross_tuple_executable_reuse_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["schema_generations"][0]["producer_domain"] = discrete(
            unversioned(1), unversioned(2)
        )
        first = manifest["capability_lanes"][0]
        first["evidence"] = [
            {
                "class": "search_nonindexing",
                "kind": "rust_test",
                "suite": "shared",
                "test": "same_test",
                "scope": "tuple",
            }
        ]
        second = copy.deepcopy(first)
        second["producer_partition"] = discrete(unversioned(2))
        manifest["capability_lanes"].append(second)
        manifest["expected_capability_lane_count"] = 2
        manifest["expected_status_rows"]["not_qualified"] = 2
        with self.assertRaisesRegex(ConformanceError, "reused across tuples"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_provider_neutral_closed_class_can_be_reused_across_tuples(self) -> None:
        manifest = minimal_manifest()
        domain = discrete(unversioned(1), unversioned(2))
        first = manifest["capability_lanes"][0]
        first["evidence"] = [
            {
                "class": "search_nonindexing",
                "kind": "rust_test",
                "suite": "shared",
                "test": "same_test",
                "scope": "provider_neutral",
            }
        ]
        second = copy.deepcopy(first)
        second["producer_partition"] = discrete(unversioned(2))
        manifest["capability_lanes"].append(second)
        manifest["expected_capability_lane_count"] = 2
        manifest["expected_status_rows"]["not_qualified"] = 2
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            contract = tracked_route_contract(root, producer_domain=domain)
            bind_manifest_contract(manifest, contract)
            inventory = validate_fixture_manifest(
                manifest,
                minimal_matrix(),
                contracts=(contract,),
                root=root,
            )
        self.assertEqual(inventory.public_executable_count, 1)

    def test_provider_neutral_tuple_class_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"][0]["evidence"] = [
            {
                "class": "exact_positive_pair",
                "kind": "rust_test",
                "suite": "shared",
                "test": "same_test",
                "scope": "provider_neutral",
            }
        ]
        with self.assertRaisesRegex(ConformanceError, "cannot be provider-neutral"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_arbitrary_evidence_class_is_rejected(self) -> None:
        manifest = minimal_manifest()
        manifest["capability_lanes"][0]["evidence"] = [
            {
                "class": "looks_good_to_me",
                "kind": "rust_test",
                "suite": "fixture_public",
                "test": "not_a_closed_claim",
                "scope": "tuple",
            }
        ]
        with self.assertRaisesRegex(ConformanceError, "unknown evidence class"):
            validate_fixture_manifest(manifest, minimal_matrix())

    def test_coordinated_manifest_and_metadata_overclaim_is_rejected(self) -> None:
        manifest, _ = supported_manifest()
        lane = manifest["capability_lanes"][0]
        public_classes = REQUIRED_EVIDENCE_CLASSES
        for evidence in lane["evidence"]:
            if evidence["kind"] == "rust_test":
                evidence["suite"] = "one_test_suite"
                evidence["test"] = "one_test_claims_everything"
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with self.assertRaisesRegex(ConformanceError, "multiple evidence classes"):
                run_fixture_conformance(
                    manifest,
                    minimal_matrix(),
                    {
                        "one_test_suite": fake_test_binary(
                            root, "one", "ok", ["one_test_claims_everything"]
                        )
                    },
                    {
                        "one_test_suite": {
                            "one_test_claims_everything": public_classes
                        }
                    },
                    root / "tmp",
                )

    def test_suite_binding_requires_physical_bazel_target_identity(self) -> None:
        with self.assertRaisesRegex(ConformanceError, "ID=TARGET=PATH"):
            _parse_suite_bindings(["alias=/tmp/test-binary"], "--suite")

    def test_suite_alias_binding_marks_selected_inventory_and_stays_unique(self) -> None:
        selected = _parse_suite_bindings(
            ["alias=//tests:unit=/tmp/test-binary"],
            "--suite-alias",
            selected_inventory=True,
        )
        self.assertTrue(selected["alias"].selected_inventory)
        with self.assertRaisesRegex(ConformanceError, "duplicate suite binding IDs"):
            _merge_suite_bindings(selected, selected)

    def test_coordinated_aliases_cannot_reuse_one_physical_target(self) -> None:
        manifest = minimal_manifest()
        lane = manifest["capability_lanes"][0]
        lane["evidence"] = [
            {
                "class": "search_nonindexing",
                "kind": "rust_test",
                "suite": "alias_a",
                "test": "search_test",
                "scope": "tuple",
            },
            {
                "class": "privacy_sinks",
                "kind": "rust_test",
                "suite": "alias_b",
                "test": "privacy_test",
                "scope": "tuple",
            },
        ]
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            target = "//tests:one_physical_target"
            with self.assertRaisesRegex(
                ConformanceError, "duplicate physical Bazel target binding"
            ):
                run_fixture_conformance(
                    manifest,
                    minimal_matrix(),
                    {
                        "alias_a": fake_test_binary(
                            root, "alias-a", "ok", ["search_test"], target=target
                        ),
                        "alias_b": fake_test_binary(
                            root, "alias-b", "ok", ["privacy_test"], target=target
                        ),
                    },
                    {
                        "alias_a": {"search_test": {"search_nonindexing"}},
                        "alias_b": {"privacy_test": {"privacy_sinks"}},
                    },
                    root / "tmp",
                )

    def test_ten_aliases_cannot_turn_one_test_binary_into_ten_classes(self) -> None:
        manifest, _ = supported_manifest()
        public_classes = sorted(REQUIRED_EVIDENCE_CLASSES)
        public_evidence = [
            evidence
            for evidence in manifest["capability_lanes"][0]["evidence"]
            if evidence["kind"] == "rust_test"
        ]
        capabilities = {}
        for index, (evidence, evidence_class) in enumerate(
            zip(public_evidence, public_classes, strict=True)
        ):
            suite_id = f"alias_{index}"
            evidence["suite"] = suite_id
            evidence["test"] = "one_test"
            capabilities[suite_id] = {"one_test": {evidence_class}}

        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            physical = fake_test_binary(
                root, "one-physical", "ok", ["one_test"]
            ).binary
            aliases = {
                suite_id: SuiteBinding(
                    target=f"//tests:{suite_id}", binary=physical
                )
                for suite_id in capabilities
            }
            with self.assertRaisesRegex(
                ConformanceError, "duplicate physical test binary binding"
            ):
                run_fixture_conformance(
                    manifest,
                    minimal_matrix(),
                    aliases,
                    capabilities,
                    root / "tmp",
                )

    def test_public_validation_accepts_supported_lanes(self) -> None:
        manifest, public_capabilities = supported_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            inventory = run_fixture_conformance(
                manifest,
                minimal_matrix(),
                {
                    "fixture_public": fake_test_binary(
                        root, "public", "ok", sorted(public_capabilities)
                    )
                },
                {"fixture_public": public_capabilities},
                root / "tmp",
            )
        self.assertEqual(inventory.status_rows["supported"], 1)

    def test_missing_or_stale_public_capability_binding_is_rejected(self) -> None:
        manifest, public_capabilities = supported_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            binaries = {
                "fixture_public": fake_test_binary(
                    root, "public", "ok", sorted(public_capabilities)
                )
            }
            with self.assertRaisesRegex(ConformanceError, "capability bindings.*missing"):
                run_fixture_conformance(
                    manifest,
                    minimal_matrix(),
                    binaries,
                    {},
                    root / "tmp-missing",
                )
            stale = copy.deepcopy(public_capabilities)
            stale["stale_test"] = {"search_nonindexing"}
            with self.assertRaisesRegex(ConformanceError, "capability test inventory.*stale"):
                run_fixture_conformance(
                    manifest,
                    minimal_matrix(),
                    binaries,
                    {"fixture_public": stale},
                    root / "tmp-stale",
                )

    def test_unclaimed_binary_test_is_rejected(self) -> None:
        manifest, public_capabilities = supported_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with self.assertRaisesRegex(ConformanceError, "unclaimed"):
                run_fixture_conformance(
                    manifest,
                    minimal_matrix(),
                    {
                        "fixture_public": fake_test_binary(
                            root,
                            "public",
                            "ok",
                            sorted(public_capabilities) + ["unclaimed"],
                        )
                    },
                    {"fixture_public": public_capabilities},
                    root / "tmp",
                )

    def test_selected_inventory_alias_runs_only_closed_claimed_tests(self) -> None:
        manifest = minimal_manifest()
        lane = manifest["capability_lanes"][0]
        lane["evidence"] = [
            {
                "class": "search_nonindexing",
                "kind": "rust_test",
                "suite": "selected",
                "test": "selected_test",
                "scope": "tuple",
            }
        ]
        capabilities = {"selected_test": {"search_nonindexing"}}
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            inventory = run_fixture_conformance(
                manifest,
                minimal_matrix(),
                {
                    "selected": fake_test_binary(
                        root,
                        "selected",
                        "ok",
                        ["selected_test", "unrelated_test"],
                        selected_inventory=True,
                    )
                },
                {"selected": capabilities},
                root / "tmp",
            )
        self.assertEqual(inventory.public_executable_count, 1)

    def test_ignored_or_zero_pass_execution_is_rejected(self) -> None:
        manifest, public_capabilities = supported_manifest()
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            for mode, message in [("ignored", "ignored"), ("zero", "pass count 0")]:
                with self.subTest(mode=mode), self.assertRaisesRegex(
                    ConformanceError, message
                ):
                    run_fixture_conformance(
                        manifest,
                        minimal_matrix(),
                        {
                            "fixture_public": fake_test_binary(
                                root,
                                f"public-{mode}",
                                mode,
                                sorted(public_capabilities),
                            )
                        },
                        {"fixture_public": public_capabilities},
                        root / f"tmp-{mode}",
                    )


if __name__ == "__main__":
    unittest.main()

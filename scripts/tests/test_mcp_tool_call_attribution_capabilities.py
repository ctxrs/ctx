#!/usr/bin/env python3
"""Focused mutation tests for the exact MCP attribution capability checker."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from base64 import b64encode
from pathlib import Path
from urllib.error import HTTPError


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.dont_write_bytecode = True

from check_mcp_tool_call_attribution_capabilities_lib import (  # noqa: E402
    CapabilityError,
    CONFORMANCE_MANIFEST,
    CONFORMANCE_SUITES,
    PUBLIC_DOC_PATHS,
    discover_public_checker_source_paths,
    load_contract,
    public_boundary_violation,
    validate_conformance_authority,
    validate_contract,
    validate_evidence_url_reachable,
    validate_public_checker_sources,
    validate_public_docs,
)


REPO_ROOT = Path(__file__).resolve().parents[2]


def boundary_class_probes() -> dict[str, str]:
    separator = "-"
    phrases = {
        "maintainer home path": "home maintainer code project".split(),
        "private repository name": "sample private".split(),
        "internal multi-repository workspace name": "sample multi repo workspace".split(),
        "internal conformance proof-packet path": (
            "adapter conformance proof packets".split()
        ),
    }
    home = Path("/").joinpath(*phrases["maintainer home path"]).as_posix()
    private_repo = separator.join(phrases["private repository name"])
    workspace = separator.join(phrases["internal multi-repository workspace name"])
    proof_words = phrases["internal conformance proof-packet path"]
    proof_path = f"{separator.join(proof_words[:2])}/{separator.join(proof_words[2:])}"
    return {
        "maintainer home path": home,
        "private repository name": private_repo,
        "internal multi-repository workspace name": workspace,
        "internal conformance proof-packet path": proof_path,
    }


def attack_forms(value: str) -> dict[str, str]:
    split_at = max(1, len(value) // 2)
    left, right = value[:split_at], value[split_at:]
    character_at = next(
        index for index, character in enumerate(value) if character.isalpha()
    )
    character = value[character_at]
    escaped = (
        value[:character_at]
        + "\\x"
        + f"{ord(character):02x}"
        + value[character_at + 1 :]
    )
    regex = value[:character_at] + f"[{character}]" + value[character_at + 1 :]
    return {
        "literal": value,
        "python-direct-plus": f"{left!r} + {right!r}",
        "python-adjacent": f"({left!r} {right!r})",
        "python-named-plus": f"left={left!r}\nright={right!r}\nprobe=left+right",
        "python-named-list-join": (
            f"parts=[{left!r}, {right!r}]\nprobe=''.join(parts)"
        ),
        "python-reversed": f"{value[::-1]!r}[::-1]",
        "shell-append": f"probe={left!r}\nprobe+={right!r}",
        "shell-named": (
            f"left={left!r}\nright={right!r}\nprobe=\"${{left}}${{right}}\""
        ),
        "escaped": escaped,
        "regex-singleton": regex,
        "base64": b64encode(value.encode("utf-8")).decode("ascii"),
    }


def authority_fixture(capability: dict[str, object]) -> dict[str, str]:
    del capability
    return {
        relative: (REPO_ROOT / relative).read_text(encoding="utf-8")
        for relative in (CONFORMANCE_MANIFEST, CONFORMANCE_SUITES)
    }


class FakeResponse:
    def __init__(self, status: int) -> None:
        self.status = status

    def __enter__(self) -> "FakeResponse":
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def getcode(self) -> int:
        return self.status


class CapabilityMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.support, self.capability, self.docs = copy.deepcopy(load_contract())

    def validate(self) -> dict[str, object]:
        return validate_contract(
            self.support,
            self.capability,
            self.docs,
            link_checker=lambda _url: None,
        )

    def assert_invalid(self, expected: str) -> None:
        with self.assertRaisesRegex(CapabilityError, expected):
            self.validate()

    def exact_row(self, provider_id: str) -> dict[str, object]:
        return next(
            row
            for row in self.capability["routes"]
            if row["provider_id"] == provider_id and row["status"] == "exact"
        )

    def test_baseline_resolves_exact_arithmetic_suites_tests_and_links(self) -> None:
        self.assertEqual(
            self.validate(),
            {
                "providers": 41,
                "base_routes": 43,
                "capability_lanes": 46,
                "lane_statuses": {"exact": 3, "not-qualified": 42, "excluded": 1},
                "provider_statuses": {"exact": 3, "not-qualified": 38, "excluded": 0},
                "exact_suites": 2,
                "exact_tests": 9,
                "exact_links": 4,
                "conformance_authority": "validated",
            },
        )

    def test_opaque_bound_kind_is_rejected(self) -> None:
        self.capability["routes"][0]["producer_bound"] = {
            "kind": "opaque",
            "value": "current",
            "unknown_generations": "not-qualified",
        }
        self.assert_invalid("outside the closed grammar")

    def test_opaque_bound_value_is_rejected(self) -> None:
        row = next(row for row in self.capability["routes"] if row["provider_id"] == "pi")
        row["producer_bound"]["versions"] = ["current"]
        self.assert_invalid("outside the closed grammar")

    def test_duplicate_full_capability_tuple_is_rejected(self) -> None:
        self.capability["routes"].append(copy.deepcopy(self.capability["routes"][0]))
        self.capability["expected_counts"]["capability_lanes"] += 1
        self.capability["expected_counts"]["lane_statuses"]["exact"] += 1
        self.assert_invalid("duplicate full capability tuple")

    def test_nonexistent_suite_is_rejected(self) -> None:
        self.capability["exact_checks"][0]["suite_id"] = (
            "ctx-history-capture::test:nonexistent"
        )
        self.assert_invalid("does not resolve through the Rust target inventory")

    def test_nonexistent_test_is_rejected(self) -> None:
        self.capability["exact_checks"][0]["tests"][0]["id"] = "nonexistent_exact_test"
        self.assert_invalid(r"does not name a #\[test\] function")

    def test_stale_helper_claim_is_rejected(self) -> None:
        self.capability["exact_checks"][0]["tests"][0]["id"] = "mcp_result"
        self.assert_invalid(r"does not name a #\[test\] function")

    def test_extra_schema_lane_cannot_rewrite_frozen_arithmetic(self) -> None:
        row = copy.deepcopy(
            next(row for row in self.capability["routes"] if row["provider_id"] == "codebuddy")
        )
        row["source_schema"] = "ide-structured-message-v2"
        row["producer_bound"] = {
            "kind": "versions",
            "versions": ["2.0.0"],
            "ranges": [],
            "source_commits": [],
            "unknown_generations": "not-qualified",
        }
        self.capability["routes"].append(row)
        self.capability["expected_counts"]["capability_lanes"] += 1
        self.capability["expected_counts"]["lane_statuses"]["not-qualified"] += 1
        self.assert_invalid("exact arithmetic")

    def test_codex_exact_partition_rejects_semver_and_other_generations(self) -> None:
        row = self.exact_row("codex")
        check = self.capability["exact_checks"][0]
        producer_bounds = [
            {
                "kind": "versions",
                "versions": [version],
                "ranges": [],
                "source_commits": ["60c722e07514d46d980034319dfcbfe4e74e659f"],
                "unknown_generations": "not-qualified",
            }
            for version in ("0.200.0", "0.201.0", "0.202.0")
        ]
        producer_bounds.append(
            {"kind": "unversioned", "generation": 2, "unknown_generations": "not-qualified"},
        )
        for producer_bound in producer_bounds:
            with self.subTest(producer_bound=producer_bound):
                original_row = copy.deepcopy(row["producer_bound"])
                original_check = copy.deepcopy(check["producer_bound"])
                row["producer_bound"] = copy.deepcopy(producer_bound)
                check["producer_bound"] = copy.deepcopy(producer_bound)
                with self.assertRaises(CapabilityError):
                    self.validate()
                row["producer_bound"] = original_row
                check["producer_bound"] = original_check

    def test_each_observed_codex_semver_lane_must_remain_not_qualified(self) -> None:
        rows = [
            row
            for row in self.capability["routes"]
            if row["provider_id"] == "codex"
            and row["producer_bound"]["kind"] == "versions"
        ]
        self.assertEqual(
            {row["producer_bound"]["versions"][0] for row in rows},
            {"0.200.0", "0.201.0", "0.202.0"},
        )
        for row in rows:
            with self.subTest(version=row["producer_bound"]["versions"][0]):
                row["status"] = "exact"
                del row["reason"]
                with self.assertRaises(CapabilityError):
                    self.validate()
                row["status"] = "not-qualified"
                row["reason"] = "writer_version_unproven"

    def test_codex_history_cannot_be_promoted(self) -> None:
        row = next(
            row
            for row in self.capability["routes"]
            if row["provider_id"] == "codex"
            and row["source_format"] == "codex_history_jsonl"
        )
        row["status"] = "exact"
        del row["reason"]
        self.assert_invalid("Codex history")

    def test_invalid_exact_evidence_url_is_rejected(self) -> None:
        self.exact_row("codex")["evidence"] = ["https://github.com/openai/codex"]
        self.assert_invalid("not a pinned GitHub blob/tree URL")

    def test_stale_parser_revision_is_rejected(self) -> None:
        self.capability["exact_checks"][1]["parser_revision"] = (
            "warp-source-backed-logical-v3"
        )
        self.assert_invalid("parser_revision is stale")

    def test_conformance_suite_alias_is_frozen(self) -> None:
        self.capability["exact_checks"][0]["conformance_suite"] = "unbound_suite"
        self.assert_invalid("authoritative suite")

    def test_conformance_authority_declaration_is_frozen(self) -> None:
        self.capability["conformance_authority"]["manifest_schema_version"] += 1
        self.assert_invalid("real manifest and suite registry")

    def test_present_conformance_authority_is_cross_checked(self) -> None:
        overrides = authority_fixture(self.capability)
        self.assertTrue(
            validate_conformance_authority(
                self.capability, authority_overrides=overrides
            )
        )
        manifest = json.loads(overrides[CONFORMANCE_MANIFEST])
        supported = next(
            lane
            for lane in manifest["capability_lanes"]
            if lane["provider"] == "codex"
            and lane["status"]["kind"] == "supported"
        )
        supported["producer_partition"]["versions"] = [
            {"kind": "semver", "version": "9.9.9"}
        ]
        mutated = dict(overrides)
        mutated[CONFORMANCE_MANIFEST] = json.dumps(manifest)
        with self.assertRaisesRegex(CapabilityError, "exact Codex generation"):
            validate_conformance_authority(
                self.capability, authority_overrides=mutated
            )

    def test_all_manifest_lanes_and_real_suite_membership_are_hash_bound(self) -> None:
        overrides = authority_fixture(self.capability)
        manifest = json.loads(overrides[CONFORMANCE_MANIFEST])
        mutations = []

        warp = next(
            lane
            for lane in manifest["capability_lanes"]
            if lane["provider"] == "warp" and lane["status"]["kind"] == "supported"
        )
        warp_generation = copy.deepcopy(manifest)
        next(
            lane
            for lane in warp_generation["capability_lanes"]
            if lane["provider"] == "warp" and lane["status"]["kind"] == "supported"
        )["producer_partition"]["versions"][0]["generation"] = 99
        mutations.append(
            (CONFORMANCE_MANIFEST, json.dumps(warp_generation), "content hash mismatch")
        )

        copilot_schema = copy.deepcopy(manifest)
        next(
            lane
            for lane in copilot_schema["capability_lanes"]
            if lane["provider"] == "copilot_cli"
            and lane["status"]["kind"] == "supported"
        )["format_schema"]["revision"] = 999
        mutations.append(
            (CONFORMANCE_MANIFEST, json.dumps(copilot_schema), "content hash mismatch")
        )

        route_drift = copy.deepcopy(manifest)
        next(
            lane
            for lane in route_drift["capability_lanes"]
            if lane["provider"] == "warp" and lane["status"]["kind"] == "supported"
        )["route"] = "fabricated_route"
        mutations.append(
            (CONFORMANCE_MANIFEST, json.dumps(route_drift), "tuple projection")
        )

        producer_drift = copy.deepcopy(manifest)
        next(
            lane
            for lane in producer_drift["capability_lanes"]
            if lane["provider"] == "warp" and lane["status"]["kind"] == "supported"
        )["producer_bound"]["generation"] = 99
        mutations.append(
            (CONFORMANCE_MANIFEST, json.dumps(producer_drift), "tuple projection")
        )

        fabricated_provider = copy.deepcopy(manifest)
        next(
            lane
            for lane in fabricated_provider["capability_lanes"]
            if lane["status"]["kind"] == "not_qualified"
            and lane["provider"] != "codex"
        )["provider"] = "fabricated_provider"
        mutations.append(
            (CONFORMANCE_MANIFEST, json.dumps(fabricated_provider), "tuple projection")
        )

        first_test = self.capability["exact_checks"][0]["tests"][0]["id"]
        quoted_test = json.dumps(first_test)
        suites_comment_only = overrides[CONFORMANCE_SUITES].replace(
            quoted_test, "", 1
        ) + f"\n# {quoted_test}\n"
        mutations.append(
            (CONFORMANCE_SUITES, suites_comment_only, "content hash mismatch")
        )

        self.assertEqual(warp["producer_partition"]["versions"][0]["generation"], 1)
        for path, content, error in mutations:
            with self.subTest(path=path):
                mutated = dict(overrides)
                mutated[path] = content
                with self.assertRaisesRegex(CapabilityError, error):
                    validate_conformance_authority(
                        self.capability, authority_overrides=mutated
                    )

    def test_partial_or_stale_suite_authority_fails_closed(self) -> None:
        overrides = authority_fixture(self.capability)
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(CapabilityError, "available together"):
                validate_conformance_authority(
                    self.capability,
                    repo_root=Path(temporary),
                    authority_overrides={
                        CONFORMANCE_MANIFEST: overrides[CONFORMANCE_MANIFEST]
                    },
                )
        first_test = self.capability["exact_checks"][0]["tests"][0]["id"]
        overrides[CONFORMANCE_SUITES] = overrides[CONFORMANCE_SUITES].replace(
            json.dumps(first_test), ""
        )
        with self.assertRaisesRegex(CapabilityError, "suite registry"):
            validate_conformance_authority(
                self.capability, authority_overrides=overrides
            )

    def test_contradictory_provider_docs_are_rejected(self) -> None:
        fixed = (
            "Capability revision 3 exact providers are Codex, Warp, and Copilot CLI."
        )
        self.docs["docs/providers.md"] = self.docs["docs/providers.md"].replace(
            fixed, "Only Codex is exact in capability revision 1."
        )
        self.assert_invalid("contradicts or omits the three-provider revision-3 contract")

    def test_generic_boundary_classes_reject_literal_and_obfuscated_forms(self) -> None:
        for expected, probe in boundary_class_probes().items():
            for encoding, attack in attack_forms(probe).items():
                with self.subTest(boundary=expected, encoding=encoding):
                    self.assertEqual(public_boundary_violation(attack), expected)

    def test_each_boundary_class_is_rejected_across_discovered_sources_and_docs(
        self,
    ) -> None:
        discovered = discover_public_checker_source_paths()
        self.assertIn(Path(__file__).resolve().relative_to(REPO_ROOT), discovered)
        for expected, probe in boundary_class_probes().items():
            for relative in discovered:
                with self.subTest(boundary=expected, source=relative.as_posix()):
                    source = (REPO_ROOT / relative).read_text(encoding="utf-8")
                    with self.assertRaisesRegex(CapabilityError, "source boundary"):
                        validate_public_docs(
                            self.docs,
                            {relative.as_posix(): source + f"\n# {probe}\n"},
                        )
            for doc_path in PUBLIC_DOC_PATHS:
                with self.subTest(boundary=expected, doc=doc_path):
                    docs = copy.deepcopy(self.docs)
                    docs[doc_path] += f"\n{probe}\n"
                    with self.assertRaisesRegex(
                        CapabilityError, "documentation boundary"
                    ):
                        validate_public_docs(docs)

    def test_obfuscation_matrix_fails_closed_in_source_and_docs(self) -> None:
        source_path = discover_public_checker_source_paths()[0]
        source = (REPO_ROOT / source_path).read_text(encoding="utf-8")
        doc_path = PUBLIC_DOC_PATHS[0]
        for expected, probe in boundary_class_probes().items():
            for encoding, attack in attack_forms(probe).items():
                with self.subTest(boundary=expected, encoding=encoding, surface="source"):
                    with self.assertRaisesRegex(CapabilityError, "source boundary"):
                        validate_public_docs(
                            self.docs,
                            {source_path.as_posix(): source + f"\n{attack}\n"},
                        )
                with self.subTest(boundary=expected, encoding=encoding, surface="docs"):
                    docs = copy.deepcopy(self.docs)
                    docs[doc_path] += f"\n{attack}\n"
                    with self.assertRaisesRegex(
                        CapabilityError, "documentation boundary"
                    ):
                        validate_public_docs(docs)

    def test_discovery_cannot_be_weakened_by_omitting_a_source_override(self) -> None:
        discovered = discover_public_checker_source_paths()
        target = Path(__file__).resolve().relative_to(REPO_ROOT)
        probe = next(iter(boundary_class_probes().values()))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative in discovered:
                destination = root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                text = (REPO_ROOT / relative).read_text(encoding="utf-8")
                if relative == target:
                    text += f"\n# {probe}\n"
                destination.write_text(text, encoding="utf-8")
            weakened_overrides = {
                relative.as_posix(): (root / relative).read_text(encoding="utf-8")
                for relative in discovered
                if relative != target
            }
            self.assertNotIn(target.as_posix(), weakened_overrides)
            with self.assertRaisesRegex(CapabilityError, "source boundary"):
                validate_public_checker_sources(root, weakened_overrides)

    def test_new_matching_source_is_discovered_without_an_inventory_edit(self) -> None:
        discovered = discover_public_checker_source_paths()
        probe = next(iter(boundary_class_probes().values()))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative in discovered:
                destination = root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_text(
                    (REPO_ROOT / relative).read_text(encoding="utf-8"),
                    encoding="utf-8",
                )
            added = root / "scripts/tests/mcp-tool-call-attribution-added-check.py"
            added.write_text(f"# {probe}\n", encoding="utf-8")
            self.assertIn(
                added.relative_to(root), discover_public_checker_source_paths(root)
            )
            with self.assertRaisesRegex(CapabilityError, "source boundary"):
                validate_public_checker_sources(root)


class EvidenceReachabilityTests(unittest.TestCase):
    URL = (
        "https://github.com/openai/codex/blob/"
        "60c722e07514d46d980034319dfcbfe4e74e659f/README.md"
    )

    def test_head_method_not_allowed_falls_back_to_bounded_get(self) -> None:
        calls: list[tuple[str, str | None]] = []

        def opener(request: object, timeout: int) -> FakeResponse:
            self.assertEqual(timeout, 15)
            method = request.get_method()
            calls.append((method, request.headers.get("Range")))
            if method == "HEAD":
                raise HTTPError(self.URL, 405, "method not allowed", {}, None)
            return FakeResponse(206)

        validate_evidence_url_reachable(self.URL, opener=opener)
        self.assertEqual(calls, [("HEAD", None), ("GET", "bytes=0-0")])

    def test_head_not_found_does_not_weaken_to_get(self) -> None:
        calls: list[str] = []

        def opener(request: object, timeout: int) -> FakeResponse:
            self.assertEqual(timeout, 15)
            calls.append(request.get_method())
            raise HTTPError(self.URL, 404, "not found", {}, None)

        with self.assertRaisesRegex(CapabilityError, "HEAD returned HTTP 404"):
            validate_evidence_url_reachable(self.URL, opener=opener)
        self.assertEqual(calls, ["HEAD"])


if __name__ == "__main__":
    unittest.main()

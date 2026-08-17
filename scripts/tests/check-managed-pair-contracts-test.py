#!/usr/bin/env python3
"""Deterministic positive and negative tests for managed-pair contracts."""

from __future__ import annotations

import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check-managed-pair-contracts.py"
AUTHORITY = ROOT / "contracts" / "ctx-managed-pair-release-authority-v1.json"
MANIFEST_SCHEMA = ROOT / "contracts" / "ctx-managed-pair-manifest-v1.schema.json"
RELEASE_SET_SCHEMA = ROOT / "contracts" / "ctx-managed-pair-release-set-v1.schema.json"
STATE_SCHEMA = ROOT / "contracts" / "ctx-managed-pair-state-v1.schema.json"

TEST_PRIVATE_KEY_PEM = """-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC4czAqM5XMipjl
QxTatkq8VmeS13e2aEpqT1v/XGL17o43i624H80xEbvB5tV/YzpO5N8sb4wEUj9h
yNzB5/U4S6SM/QadcA9fk/V7KeBOcz15PvZaU0UNp/dKVvzEFtxv/rjQCfA80C2N
30lTwti8pts4IulxVeB7BkIvqs3XADV5zBVwRACHWt5MKcMrXfBcmKRy8TLdNeml
lPgU3V2pj4c54KQ0aoy3/970+ry3P+eT8BlatU4k8R+pS0Oy4s3Ezczj9UrPCREd
1m2tAqaw8B0wRoei+nHEPWqbbzgx8fepv38U9LXmzYpCjSWSZ+zcZ4YBsXlyab3a
2PjyZ42HAgMBAAECggEAHQvis1qhRe8zibMJJzIazdLrh5fP3dVJlrk9mxag7Oqu
0bd42WyEoywQPcZMq71kEsV/EZ/VVF7hZVQ803pkRwO+e4djEcryWNJTj5w2GxSR
wzSzleDUGITxb+8H6hdRin95+iT+hI0iB1v4z6x49ihukEYLLhJgge8n4BrNRISa
P+SInTo/UzO5NIzh8HdQBJqkammS4c/Eij0jVw9onMpOFWKAxcs0hmk1SSy6KouD
yDBqp6m6ILlAuggZutkn+7X4QUzvgBQePYy6BNX57dmFpBWt/8DVc5m4Ciwd+s1L
CLRL86X6YLtc5wTQvdX/xHbW9m/FUXk5EvK2eQ+IyQKBgQD7B4aFQFwHiRjO323d
I7FUcSgsBEz/pYiucEF5c+GQUpSq/ORgFg7sYLAv3312nbu/TdIw2O0KxhhfUX6j
iRGe5NzSogUpRHk3Rq/tbQKULezDi9Lc7ROUuMYRpsHSjiVLB+zYdRDZULBqAdSo
3A0c0/xfCKB0efIJt4SfTVtcvwKBgQC8Git0ry8csFgmwmuxHL1nBmxXBLyZ04Ko
PQ+WyLPgL8cVP3Bf19zXDtmeoPSD8bZODys4UKit3zpZDEKN9S8JeN2E1h5MTgKN
wmOxdimAo0xKHJ/EnvxzfR5UzbrGiuajCFvIDPjItl3gSJ2av1cwQ8ljZBtOoqdX
KiTNCw7ZOQKBgQCTEuSom32P2K4VPmiC4M+blrSfnWFzgoujEBf8TX2BbjC2QXaY
KTRTH476bWl3npCKU9DrV50B6/AJoJievcb6HkKWkeCOPhT64speQ7j4EjQemYRQ
dgI750n8u4PhlfCZlioY4/WcLR8+7JWo3Uw9cKHzF/3SYEQDl2b3Yn49xwKBgFda
g+HNVUCqeFWPpnl60k6dAgUrUvbQ7fV5Xdr1W+t55KdubZ5k3c8Vu2RadRMtVi9M
BhNCCgOtDii6c9H/EhgBBEajNTDUbYUtyCRqrn1p2Iz2XA/wkWaErWhOnjWD3fXK
dO0jcQms/02gC2kJANGOOWEp5TCQgswM60g5oWypAoGADlZTP+97w9NcOJoQdZVi
+I5NLRKHUjAvax4BALtH5uuVIwj6cSwheRkBzd7rU1aQ65yuUYwIznDsC2rir26x
ehIUvhTehZf04otZbIo7UUvFhohRmX5k4/Idf/njMa/dA5afBMM1xE7IkoeHQyLc
3I9zapKTmyq90XvKHvA9eyA=
-----END PRIVATE KEY-----"""

TEST_PUBLIC_KEY_PEM = """-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAuHMwKjOVzIqY5UMU2rZKvFZnktd3tmhKak9b/1xi9e6ON4utuB/N
MRG7webVf2M6TuTfLG+MBFI/Ycjcwef1OEukjP0GnXAPX5P1eyngTnM9eT72WlNF
Daf3Slb8xBbcb/640AnwPNAtjd9JU8LYvKbbOCLpcVXgewZCL6rN1wA1ecwVcEQA
h1reTCnDK13wXJikcvEy3TXppZT4FN1dqY+HOeCkNGqMt//e9Pq8tz/nk/AZWrVO
JPEfqUtDsuLNxM3M4/VKzwkRHdZtrQKmsPAdMEaHovpxxD1qm284MfH3qb9/FPS1
5s2KQo0lkmfs3GeGAbF5cmm92tj48meNhwIDAQAB
-----END RSA PUBLIC KEY-----"""

SPEC = importlib.util.spec_from_file_location("managed_pair_contracts", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load managed pair contract checker")
contracts = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contracts)


def digest(label: str) -> str:
    return hashlib.sha256(label.encode("utf-8")).hexdigest()


def test_key_pem(name: str) -> str:
    return {
        "TEST_PRIVATE_KEY_PEM": TEST_PRIVATE_KEY_PEM,
        "TEST_PUBLIC_KEY_PEM": TEST_PUBLIC_KEY_PEM,
    }[name]


def test_authorities() -> dict[str, dict[str, str]]:
    public_key = test_key_pem("TEST_PUBLIC_KEY_PEM")
    return {
        "stable": {"key_id": "test-stable", "signature_algorithm": "rsa-pkcs1v15-sha256", "public_key_pem": public_key},
        "staging": {"key_id": "test-staging", "signature_algorithm": "rsa-pkcs1v15-sha256", "public_key_pem": public_key},
    }


def private_numbers() -> tuple[int, int]:
    pem = test_key_pem("TEST_PRIVATE_KEY_PEM")
    der = __import__("base64").b64decode("".join(pem.splitlines()[1:-1]), validate=True)
    tag, sequence, end = contracts.der_tlv(der, 0)
    assert tag == 0x30 and end == len(der)
    offset = 0
    _, _, offset = contracts.der_tlv(sequence, offset)
    _, _, offset = contracts.der_tlv(sequence, offset)
    tag, private_der, offset = contracts.der_tlv(sequence, offset)
    assert tag == 0x04 and offset == len(sequence)
    tag, private_sequence, end = contracts.der_tlv(private_der, 0)
    assert tag == 0x30 and end == len(private_der)
    integers = []
    offset = 0
    while offset < len(private_sequence):
        tag, encoded, offset = contracts.der_tlv(private_sequence, offset)
        assert tag == 0x02
        integers.append(int.from_bytes(encoded, "big"))
    return integers[1], integers[3]


def envelope(payload: dict[str, object]) -> dict[str, object]:
    payload_bytes = contracts.canonical_payload_bytes(payload)
    modulus, private_exponent = private_numbers()
    key_bytes = (modulus.bit_length() + 7) // 8
    digest_info = bytes.fromhex("3031300d060960864801650304020105000420") + hashlib.sha256(payload_bytes).digest()
    encoded_message = b"\x00\x01" + b"\xff" * (key_bytes - len(digest_info) - 3) + b"\x00" + digest_info
    signature = pow(int.from_bytes(encoded_message, "big"), private_exponent, modulus).to_bytes(key_bytes, "big")
    return {
        "schema_version": 1,
        "manifest_base64": base64.b64encode(payload_bytes).decode("ascii"),
        "signature_base64": base64.b64encode(signature).decode("ascii"),
    }


def manifest(target_id: str) -> dict[str, object]:
    target = next(target for target in contracts.load_matrix()["targets"] if target["id"] == target_id)

    def component(kind: str, artifact: str, slot: str, rust_target: str) -> dict[str, object]:
        artifact_digest = digest(f"{target_id}-{kind}")
        return {
            "artifact_name": artifact,
            "object_key": f"sha256/{artifact_digest}/{artifact}",
            "sha256": artifact_digest,
            "size_bytes": 4096,
            "install_slot": f"<install-root>/{slot}",
            "build_identity": {
                "component": kind,
                "rust_target": rust_target,
                "source_revision": "a" * 40,
                "build_fingerprint": digest(f"{target_id}-{kind}-build"),
            },
        }

    return {
        "contract": "ctx-managed-pair-manifest",
        "schema_version": 1,
        "channel": "staging",
        "release_authority_key_id": contracts.load_authority_registry()["staging"]["key_id"],
        "release_name": "v1.2.3",
        "target": {
            "id": target_id,
            "os": target["os"],
            "arch": target["arch"],
            "core_rust_target": target["public_rust_target"],
            "companion_rust_target": target["official_companion_rust_target"],
        },
        "install_geometry": {
            "install_root": "<install-root>",
            "managed_bin_dir": "<install-root>/bin",
            "core_slot": f"<install-root>/{target['managed_pair_core_slot']}",
            "companion_slot": f"<install-root>/{target['managed_pair_companion_slot']}",
        },
        "target_matrix_sha256": contracts.target_matrix_sha256(),
        "rollback_generation": 17,
        "snapshot": {"contract": "ctx-managed-pair-snapshot-v1", "fingerprint": digest("snapshot")},
        "compatibility": {"invocation_fingerprint": digest("invoke"), "core_capability_fingerprint": digest("core-capabilities")},
        "components": {
            "core": component("core", target["public_artifact"], target["managed_pair_core_slot"], target["public_rust_target"]),
            "companion": component("companion", target["helper_artifact"], target["managed_pair_companion_slot"], target["official_companion_rust_target"]),
        },
    }


def release_set() -> dict[str, object]:
    target_manifests = []
    for target_id in contracts.TARGET_IDS:
        manifest_digest = digest(f"{target_id}-manifest")
        name = f"ctx-managed-pair-{target_id}.json"
        target_manifests.append(
            {
                "target_id": target_id,
                "manifest_name": name,
                "manifest_object_key": f"sha256/{manifest_digest}/{name}",
                "manifest_sha256": manifest_digest,
                "manifest_size_bytes": 512,
            }
        )
    return {
        "contract": "ctx-managed-pair-release-set",
        "schema_version": 1,
        "channel": "staging",
        "release_authority_key_id": contracts.load_authority_registry()["staging"]["key_id"],
        "release_name": "v1.2.3",
        "target_matrix_sha256": contracts.target_matrix_sha256(),
        "rollback_generation": 17,
        "snapshot": {"contract": "ctx-managed-pair-snapshot-v1", "fingerprint": digest("snapshot")},
        "compatibility": {"invocation_fingerprint": digest("invoke"), "core_capability_fingerprint": digest("core-capabilities")},
        "target_manifests": target_manifests,
    }


def envelope_payload(value: dict[str, object]) -> dict[str, object]:
    payload = json.loads(base64.b64decode(value["manifest_base64"], validate=True))
    assert isinstance(payload, dict)
    return payload


def release_bundle(
    manifests_by_target: dict[str, dict[str, object]] | None = None,
) -> tuple[dict[str, object], dict[str, bytes]]:
    manifests_by_target = (
        {target_id: manifest(target_id) for target_id in contracts.TARGET_IDS}
        if manifests_by_target is None
        else manifests_by_target
    )
    references = []
    manifest_envelopes: dict[str, bytes] = {}
    authorities = test_authorities()
    for target_id in contracts.TARGET_IDS:
        value = manifests_by_target[target_id]
        value["release_authority_key_id"] = authorities[value["channel"]]["key_id"]
        encoded = contracts.canonical_payload_bytes(envelope(value))
        manifest_digest = hashlib.sha256(encoded).hexdigest()
        name = f"ctx-managed-pair-{target_id}.json"
        object_key = f"sha256/{manifest_digest}/{name}"
        references.append(
            {
                "target_id": target_id,
                "manifest_name": name,
                "manifest_object_key": object_key,
                "manifest_sha256": manifest_digest,
                "manifest_size_bytes": len(encoded),
            }
        )
        manifest_envelopes[object_key] = encoded
    value = release_set()
    value["release_authority_key_id"] = authorities[value["channel"]]["key_id"]
    value["target_manifests"] = references
    return envelope(value), manifest_envelopes


def installed_state() -> dict[str, object]:
    return {
        "contract": "ctx-managed-pair-state",
        "schema_version": 1,
        "identity": {
            "release_name": "v1.2.3",
            "target": "linux-x64",
            "rollback_generation": 17,
            "manifest_sha256": digest("manifest"),
            "core": {"sha256": digest("core"), "size_bytes": 123},
            "companion": {"sha256": digest("companion"), "size_bytes": 456},
        },
        "envelope_sha256": digest("envelope"),
        "envelope_size_bytes": 789,
    }


class ManagedPairContractsTest(unittest.TestCase):
    def test_positive_five_target_contracts(self) -> None:
        authorities = contracts.load_authority_registry()
        matrix = contracts.load_matrix()
        for target_id in contracts.TARGET_IDS:
            contracts.validate_manifest(manifest(target_id), matrix=matrix, authorities=authorities)
        contracts.validate_release_set(release_set(), authorities=authorities)

        signed_release_set, manifest_envelopes = release_bundle()
        validated_release_set, validated_manifests = contracts.validate_release_bundle(
            signed_release_set,
            manifest_envelopes,
            matrix=matrix,
            authorities=test_authorities(),
        )
        self.assertEqual(validated_release_set["release_name"], "v1.2.3")
        self.assertEqual(
            tuple(value["target"]["id"] for value in validated_manifests),
            contracts.TARGET_IDS,
        )

    def test_schema_documents_are_closed_and_encode_the_exact_contract(self) -> None:
        manifest_schema = json.loads(MANIFEST_SCHEMA.read_text(encoding="utf-8"))
        release_set_schema = json.loads(RELEASE_SET_SCHEMA.read_text(encoding="utf-8"))
        self.assertTrue(manifest_schema["additionalProperties"] is False)
        self.assertEqual(manifest_schema["properties"]["components"]["required"], ["core", "companion"])
        self.assertEqual(manifest_schema["properties"]["rollback_generation"]["minimum"], 1)
        self.assertNotIn("signature", manifest_schema["properties"])
        self.assertTrue(release_set_schema["additionalProperties"] is False)
        prefix_items = release_set_schema["properties"]["target_manifests"]["prefixItems"]
        self.assertEqual(
            [item["allOf"][1]["properties"]["target_id"]["const"] for item in prefix_items],
            list(contracts.TARGET_IDS),
        )
        self.assertFalse(release_set_schema["properties"]["target_manifests"]["items"])
        envelope_schema = json.loads((ROOT / "contracts" / "ctx-managed-pair-signed-envelope-v1.schema.json").read_text(encoding="utf-8"))
        self.assertEqual(envelope_schema["required"], ["schema_version", "manifest_base64", "signature_base64"])
        self.assertEqual(set(envelope_schema["properties"]), {"schema_version", "manifest_base64", "signature_base64"})

        state_schema = json.loads(STATE_SCHEMA.read_text(encoding="utf-8"))
        self.assertEqual(
            state_schema["required"],
            ["contract", "schema_version", "identity", "envelope_sha256", "envelope_size_bytes"],
        )
        self.assertEqual(
            state_schema["$defs"]["pair_identity"]["required"],
            ["release_name", "target", "rollback_generation", "manifest_sha256", "core", "companion"],
        )

    def test_installed_state_is_one_closed_nested_contract(self) -> None:
        value = installed_state()
        contracts.validate_state(value)
        flat = copy.deepcopy(value)
        flat["rollback_generation"] = flat["identity"]["rollback_generation"]  # type: ignore[index]
        with self.assertRaises(contracts.ContractError):
            contracts.validate_state(flat)
        missing_component = copy.deepcopy(value)
        del missing_component["identity"]["companion"]  # type: ignore[index]
        with self.assertRaises(contracts.ContractError):
            contracts.validate_state(missing_component)

    def test_registry_is_closed_public_stable_and_staging_material(self) -> None:
        authority = json.loads(AUTHORITY.read_text(encoding="utf-8"))
        self.assertEqual(authority["contract"], "ctx-managed-pair-release-authority")
        self.assertEqual(authority["schema_version"], 1)
        self.assertEqual(
            [entry["id"] for entry in authority["channels"]],
            ["stable", "staging"],
        )
        registry = contracts.load_authority_registry()
        self.assertEqual(list(registry), ["stable", "staging"])
        for entry in registry.values():
            self.assertEqual(entry["signature_algorithm"], "rsa-pkcs1v15-sha256")
            self.assertTrue(entry["key_id"])
            self.assertTrue(entry["public_key_pem"].startswith("-----BEGIN RSA PUBLIC KEY-----"))

    def test_custom_bin_dir_missing_component_uppercase_hash_and_mutable_key_are_rejected(self) -> None:
        cases = []
        custom_bin = manifest("linux-x64")
        custom_bin["install_geometry"]["managed_bin_dir"] = "<install-root>/custom-bin"  # type: ignore[index]
        cases.append((custom_bin, "fixed install root"))
        missing_companion = manifest("linux-x64")
        del missing_companion["components"]["companion"]  # type: ignore[index]
        cases.append((missing_companion, "components"))
        uppercase_hash = manifest("linux-x64")
        uppercase_hash["components"]["core"]["sha256"] = "A" * 64  # type: ignore[index]
        cases.append((uppercase_hash, "core hash"))
        mutable_key = manifest("linux-x64")
        mutable_key["components"]["core"]["object_key"] = "releases/latest/ctx"  # type: ignore[index]
        cases.append((mutable_key, "object key"))
        for value, message in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(contracts.ContractError, message):
                    contracts.validate_manifest(value)

    def test_windows_companion_is_independently_bound_to_msvc_not_gnu_factory_output(self) -> None:
        value = manifest("windows-x64")
        value["target"]["companion_rust_target"] = "x86_64-pc-windows-gnu"  # type: ignore[index]
        with self.assertRaisesRegex(contracts.ContractError, "official companion target"):
            contracts.validate_manifest(value)

    def test_detached_envelope_verifies_exact_canonical_payload_bytes(self) -> None:
        value = manifest("linux-x64")
        value["release_authority_key_id"] = "test-staging"
        signed = envelope(value)
        decoded = contracts.validate_envelope(signed, authorities=test_authorities())
        self.assertEqual(decoded, value)
        signature_tamper = copy.deepcopy(signed)
        signature_tamper["signature_base64"] = "A" * len(signed["signature_base64"])
        with self.assertRaisesRegex(contracts.ContractError, "does not verify"):
            contracts.validate_envelope(signature_tamper, authorities=test_authorities())

    def test_rsa_verifier_rejects_noncanonical_signature_integer_s_plus_n(self) -> None:
        value = manifest("linux-x64")
        value["release_authority_key_id"] = "test-staging"
        modulus, _ = contracts.rsa_public_numbers(test_authorities()["staging"]["public_key_pem"])
        for patch in range(100):
            value["release_name"] = f"v1.2.{patch}"
            signed = envelope(value)
            signature = base64.b64decode(signed["signature_base64"], validate=True)
            noncanonical = int.from_bytes(signature, "big") + modulus
            if noncanonical < 1 << (len(signature) * 8):
                break
        else:
            self.fail("could not construct a fixed-width noncanonical RSA integer")
        signed["signature_base64"] = base64.b64encode(noncanonical.to_bytes(len(signature), "big")).decode("ascii")
        self.assertFalse(
            contracts.verify_detached_signature(
                contracts.canonical_payload_bytes(value),
                noncanonical.to_bytes(len(signature), "big"),
                test_authorities()["staging"]["public_key_pem"],
            )
        )
        with self.assertRaisesRegex(contracts.ContractError, "does not verify"):
            contracts.validate_envelope(signed, authorities=test_authorities())

    def test_detached_envelope_rejects_malformed_cross_channel_and_circular_payloads(self) -> None:
        value = manifest("linux-x64")
        value["release_authority_key_id"] = "test-staging"
        signed = envelope(value)
        malformed = copy.deepcopy(signed)
        malformed["manifest_base64"] = "not base64!"
        with self.assertRaisesRegex(contracts.ContractError, "manifest_base64"):
            contracts.validate_envelope(malformed, authorities=test_authorities())
        cross_channel = copy.deepcopy(value)
        cross_channel["channel"] = "stable"
        with self.assertRaisesRegex(contracts.ContractError, "authority key ID does not match"):
            contracts.validate_envelope(envelope(cross_channel), authorities=test_authorities())
        circular = copy.deepcopy(value)
        circular["signature"] = {"value": "must-not-be-in-payload"}
        with self.assertRaisesRegex(contracts.ContractError, "missing or unexpected"):
            contracts.validate_envelope(envelope(circular), authorities=test_authorities())
        entitlement = copy.deepcopy(value)
        entitlement["entitlement_key"] = "must-not-be-in-public-contract"
        with self.assertRaisesRegex(contracts.ContractError, "missing or unexpected"):
            contracts.validate_envelope(envelope(entitlement), authorities=test_authorities())
        noncompact_bytes = json.dumps(value, separators=(",", ": "), sort_keys=False).encode("utf-8")
        noncompact = envelope(value)
        noncompact["manifest_base64"] = __import__("base64").b64encode(noncompact_bytes).decode("ascii")
        with self.assertRaisesRegex(contracts.ContractError, "compact canonical JSON"):
            contracts.validate_envelope(noncompact, authorities=test_authorities())
        value = manifest("windows-x64")
        value["components"]["companion"]["build_identity"]["rust_target"] = "x86_64-pc-windows-gnu"  # type: ignore[index]
        with self.assertRaisesRegex(contracts.ContractError, "companion build identity"):
            contracts.validate_manifest(value)

    def test_release_set_rejects_target_order_extra_surface_and_rollback(self) -> None:
        value = release_set()
        value["target_manifests"][0], value["target_manifests"][1] = value["target_manifests"][1], value["target_manifests"][0]  # type: ignore[index]
        with self.assertRaisesRegex(contracts.ContractError, "exact five-target ordered matrix"):
            contracts.validate_release_set(value)
        value = release_set()
        value["acquisition_url"] = "https://example.invalid"  # type: ignore[index]
        with self.assertRaisesRegex(contracts.ContractError, "missing or unexpected fields"):
            contracts.validate_release_set(value)
        with self.assertRaisesRegex(contracts.ContractError, "lower than retained state"):
            contracts.validate_release_set(release_set(), retained_rollback_generation=18)

    def test_release_bundle_rejects_mixed_release_identity(self) -> None:
        cases = (
            ("channel", lambda value: value.update(channel="stable")),
            ("release name", lambda value: value.update(release_name="v1.2.4")),
            ("rollback generation", lambda value: value.update(rollback_generation=18)),
            ("snapshot fingerprint", lambda value: value["snapshot"].update(fingerprint=digest("other-snapshot"))),
            (
                "invocation compatibility fingerprint",
                lambda value: value["compatibility"].update(invocation_fingerprint=digest("other-invocation")),
            ),
            (
                "Core-capability compatibility fingerprint",
                lambda value: value["compatibility"].update(core_capability_fingerprint=digest("other-capabilities")),
            ),
        )
        for label, mutate in cases:
            with self.subTest(label=label):
                manifests = {target_id: manifest(target_id) for target_id in contracts.TARGET_IDS}
                mutate(manifests["linux-x64"])
                signed_release_set, manifest_envelopes = release_bundle(manifests)
                with self.assertRaisesRegex(contracts.ContractError, label):
                    contracts.validate_release_bundle(
                        signed_release_set,
                        manifest_envelopes,
                        authorities=test_authorities(),
                    )

    def test_release_bundle_rejects_missing_wrong_or_mistargeted_references(self) -> None:
        signed_release_set, manifest_envelopes = release_bundle()
        missing = dict(manifest_envelopes)
        missing.pop(next(iter(missing)))
        with self.assertRaisesRegex(contracts.ContractError, "exactly the five referenced"):
            contracts.validate_release_bundle(signed_release_set, missing, authorities=test_authorities())

        wrong_release_set = envelope_payload(signed_release_set)
        wrong_reference = wrong_release_set["target_manifests"][0]
        old_key = wrong_reference["manifest_object_key"]
        wrong_digest = "0" * 64
        wrong_key = f"sha256/{wrong_digest}/{wrong_reference['manifest_name']}"
        wrong_reference["manifest_sha256"] = wrong_digest
        wrong_reference["manifest_object_key"] = wrong_key
        wrong_envelopes = dict(manifest_envelopes)
        wrong_envelopes[wrong_key] = wrong_envelopes.pop(old_key)
        with self.assertRaisesRegex(contracts.ContractError, "hash does not match"):
            contracts.validate_release_bundle(
                envelope(wrong_release_set),
                wrong_envelopes,
                authorities=test_authorities(),
            )

        manifests = {target_id: manifest(target_id) for target_id in contracts.TARGET_IDS}
        manifests["linux-arm64"] = manifest("linux-x64")
        mistargeted_release_set, mistargeted_envelopes = release_bundle(manifests)
        with self.assertRaisesRegex(contracts.ContractError, "identity does not match"):
            contracts.validate_release_bundle(
                mistargeted_release_set,
                mistargeted_envelopes,
                authorities=test_authorities(),
            )

    def test_release_bundle_verifies_each_referenced_detached_signature(self) -> None:
        signed_release_set, manifest_envelopes = release_bundle()
        release_value = envelope_payload(signed_release_set)
        reference = release_value["target_manifests"][0]
        old_key = reference["manifest_object_key"]
        target_envelope = json.loads(manifest_envelopes[old_key])
        target_envelope["signature_base64"] = "A" * len(target_envelope["signature_base64"])
        encoded = contracts.canonical_payload_bytes(target_envelope)
        target_digest = hashlib.sha256(encoded).hexdigest()
        new_key = f"sha256/{target_digest}/{reference['manifest_name']}"
        reference["manifest_object_key"] = new_key
        reference["manifest_sha256"] = target_digest
        reference["manifest_size_bytes"] = len(encoded)
        tampered_envelopes = dict(manifest_envelopes)
        tampered_envelopes.pop(old_key)
        tampered_envelopes[new_key] = encoded
        with self.assertRaisesRegex(contracts.ContractError, "signature does not verify"):
            contracts.validate_release_bundle(
                envelope(release_value),
                tampered_envelopes,
                authorities=test_authorities(),
            )

    def test_release_bundle_rejects_duplicate_envelope_fields(self) -> None:
        signed_release_set, manifest_envelopes = release_bundle()
        reference = envelope_payload(signed_release_set)["target_manifests"][0]
        object_key = reference["manifest_object_key"]
        original = manifest_envelopes[object_key]
        duplicate = original.replace(b'{"manifest_base64":', b'{"schema_version":1,"manifest_base64":', 1)
        digest = hashlib.sha256(duplicate).hexdigest()
        name = reference["manifest_name"]
        replacement_key = f"sha256/{digest}/{name}"
        release_value = envelope_payload(signed_release_set)
        release_reference = release_value["target_manifests"][0]
        release_reference["manifest_object_key"] = replacement_key
        release_reference["manifest_sha256"] = digest
        release_reference["manifest_size_bytes"] = len(duplicate)
        replaced = dict(manifest_envelopes)
        replaced.pop(object_key)
        replaced[replacement_key] = duplicate
        with self.assertRaisesRegex(contracts.ContractError, "duplicate field"):
            contracts.validate_release_bundle(
                envelope(release_value), replaced, authorities=test_authorities()
            )

    def test_registry_rejects_key_fingerprint_or_commercial_surface_mutation(self) -> None:
        registry = json.loads(AUTHORITY.read_text(encoding="utf-8"))
        wrong_fingerprint = copy.deepcopy(registry)
        wrong_fingerprint["channels"][0]["public_key_der_sha256"] = "0" * 64
        with self.assertRaisesRegex(contracts.ContractError, "fingerprint does not match"):
            contracts.validate_authority_registry(wrong_fingerprint)
        commercial_surface = copy.deepcopy(registry)
        commercial_surface["price"] = 1
        with self.assertRaisesRegex(contracts.ContractError, "missing or unexpected fields"):
            contracts.validate_authority_registry(commercial_surface)


if __name__ == "__main__":
    unittest.main()

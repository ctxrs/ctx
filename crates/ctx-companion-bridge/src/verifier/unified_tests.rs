//! Authored, pre-signed release fixtures contain only a public test authority.
//! The production verifier never selects this authority.
use super::*;

const AUTHORITY: &[u8] =
    include_bytes!("../../../../scripts/release/tests/fixtures/unified/authority.json");
const ARTIFACT: &[u8] =
    include_bytes!("../../../../scripts/release/tests/fixtures/unified/artifact.txt");
const FIXTURES: [(&str, &[u8]); 5] = [
    (
        "linux-arm64",
        include_bytes!("../../../../scripts/release/tests/fixtures/unified/linux-arm64.json"),
    ),
    (
        "linux-x64",
        include_bytes!("../../../../scripts/release/tests/fixtures/unified/linux-x64.json"),
    ),
    (
        "macos-arm64",
        include_bytes!("../../../../scripts/release/tests/fixtures/unified/macos-arm64.json"),
    ),
    (
        "macos-x64",
        include_bytes!("../../../../scripts/release/tests/fixtures/unified/macos-x64.json"),
    ),
    (
        "windows-x64",
        include_bytes!("../../../../scripts/release/tests/fixtures/unified/windows-x64.json"),
    ),
];

fn fixture() -> &'static [u8] {
    let target = TargetSpec::current().unwrap();
    FIXTURES.iter().find(|(id, _)| *id == target.id).unwrap().1
}

#[test]
fn legacy_signed_projection_accepts_the_same_unified_bytes_in_both_slots() {
    let verified = verify_envelope(
        &ManagedPairExpectations::new(ReleaseChannel::Stable),
        AUTHORITY,
        fixture(),
    )
    .unwrap();
    assert_eq!(verified.identity.core(), verified.identity.companion());
    assert_eq!(verified.identity.core().sha256(), digest(ARTIFACT));
    assert_eq!(verified.identity.core().size_bytes(), ARTIFACT.len() as u64);
    assert_eq!(verified.identity.release_name(), "v1.5.0");
    assert_eq!(verified.identity.rollback_generation(), 27);
    // No runtime compatibility inventory or private build is consulted.
    let envelope: Envelope = serde_json::from_slice(fixture()).unwrap();
    let manifest: Manifest =
        serde_json::from_slice(&BASE64.decode(envelope.manifest_base64).unwrap()).unwrap();
    assert_eq!(
        manifest.components.core.build_identity.source_revision,
        "1".repeat(40)
    );
    assert_eq!(
        manifest.components.core.build_identity.source_revision,
        manifest.components.companion.build_identity.source_revision
    );
}

#[test]
fn fixture_authority_is_never_a_production_trust_override() {
    let stable = ManagedPairExpectations::new(ReleaseChannel::Stable);
    assert!(verify_envelope(&stable, EMBEDDED_AUTHORITY, fixture()).is_err());
    assert!(verify_envelope(
        &ManagedPairExpectations::new(ReleaseChannel::Staging),
        AUTHORITY,
        fixture()
    )
    .is_err());
    for (id, bytes) in FIXTURES {
        if id != TargetSpec::current().unwrap().id {
            assert!(
                verify_envelope(&stable, AUTHORITY, bytes).is_err(),
                "accepted {id} on another target"
            );
        }
    }
}

#[test]
fn signed_projection_detects_changed_manifest_or_signature() {
    let stable = ManagedPairExpectations::new(ReleaseChannel::Stable);
    for field in ["manifest_base64", "signature_base64"] {
        let mut envelope: Value = serde_json::from_slice(fixture()).unwrap();
        let mut bytes = BASE64.decode(envelope[field].as_str().unwrap()).unwrap();
        if field == "manifest_base64" {
            let mut manifest: Value = serde_json::from_slice(&bytes).unwrap();
            manifest["rollback_generation"] = Value::from(28);
            bytes = serde_json::to_vec(&manifest).unwrap();
        } else {
            bytes[0] ^= 1;
        }
        envelope[field] = Value::String(BASE64.encode(bytes));
        assert!(
            verify_envelope(&stable, AUTHORITY, &serde_json::to_vec(&envelope).unwrap()).is_err()
        );
    }
}

#[test]
fn legacy_manifest_keeps_matrix_slots_and_generation_bounds() {
    let envelope: Envelope = serde_json::from_slice(fixture()).unwrap();
    let original: Value =
        serde_json::from_slice(&BASE64.decode(envelope.manifest_base64).unwrap()).unwrap();
    for (pointer, replacement) in [
        ("/target_matrix_sha256", Value::String("2".repeat(64))),
        (
            "/components/companion/install_slot",
            Value::String("<install-root>/bin/other".to_owned()),
        ),
        (
            "/components/core/object_key",
            Value::String("sha256/other/ctx".to_owned()),
        ),
        ("/rollback_generation", Value::from(0)),
        (
            "/rollback_generation",
            Value::from(9_007_199_254_740_992_u64),
        ),
    ] {
        let mut document = original.clone();
        *document.pointer_mut(pointer).unwrap() = replacement;
        let manifest: Manifest = serde_json::from_value(document).unwrap();
        assert!(
            validate_manifest_identity(&manifest, TargetSpec::current().unwrap()).is_err(),
            "accepted {pointer}"
        );
    }
}

#[test]
fn authority_override_is_confined_to_rebuilt_qualification_candidates() {
    for case in [
        "missing",
        "valid",
        "invalid",
        "empty",
        "oversized",
        "tampered",
    ] {
        let authority = match case {
            "invalid" => "not an authority".to_owned(),
            "empty" => String::new(),
            "oversized" => "x".repeat(16 * 1024 + 1),
            _ => std::str::from_utf8(AUTHORITY).unwrap().to_owned(),
        };
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "verifier::unified_tests::authority_entrypoint_probe",
                "--nocapture",
            ])
            .env("CTX_LEGACY_AUTHORITY_PROBE", case)
            .env("CTX_RELEASE_MANAGED_PAIR_AUTHORITY_JSON", authority);
        if case == "missing" {
            command.env_remove("CTX_RELEASE_MANAGED_PAIR_AUTHORITY_JSON");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[test]
fn authority_entrypoint_probe() {
    let Ok(case) = std::env::var("CTX_LEGACY_AUTHORITY_PROBE") else {
        return;
    };
    let mut bytes = fixture().to_vec();
    if case == "tampered" {
        let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
        let mut signature = BASE64
            .decode(envelope["signature_base64"].as_str().unwrap())
            .unwrap();
        signature[0] ^= 1;
        envelope["signature_base64"] = Value::String(BASE64.encode(signature));
        bytes = serde_json::to_vec(&envelope).unwrap();
    }
    let result = verify_signed_managed_pair_envelope(
        &ManagedPairExpectations::new(ReleaseChannel::Stable),
        &bytes,
    );
    assert_eq!(
        result.is_ok(),
        cfg!(ctx_release_qualification) && case == "valid"
    );
    #[cfg(not(ctx_release_qualification))]
    assert!(
        !result
            .unwrap_err()
            .to_string()
            .contains("qualification authority"),
        "production must not read the override"
    );
}

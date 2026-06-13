use std::fs;
use std::path::PathBuf;

mod common;

use ctx_update_service::ReleaseManifest;

fn corpus_dir() -> PathBuf {
    common::resolve_manifest_dir().join("tests/corpus/release_manifests")
}

fn load_manifest(name: &str) -> ReleaseManifest {
    let path = corpus_dir().join(name);
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    });
    let manifest = serde_json::from_str::<ReleaseManifest>(&raw).unwrap_or_else(|err| {
        panic!("failed to parse {}: {err}", path.display());
    });
    let encoded = serde_json::to_string(&manifest).expect("manifest should serialize");
    serde_json::from_str(&encoded).expect("roundtripped manifest should parse")
}

#[test]
fn stable_release_manifest_corpus_roundtrips() {
    let manifest = load_manifest("stable-latest.json");
    assert_eq!(manifest.channel, "stable");
    assert_eq!(manifest.latest_version, "1.2.3");
    assert!(manifest.platforms.contains_key("linux-x64"));
    assert!(manifest.platforms.contains_key("macos-arm64"));
    assert!(manifest.platforms.contains_key("windows-x64"));

    let linux = manifest
        .platforms
        .get("linux-x64")
        .expect("linux-x64 platform missing");
    assert!(linux.preferred_desktop_artifact("linux-x64").is_some());

    let mac = manifest
        .platforms
        .get("macos-arm64")
        .expect("macos-arm64 platform missing");
    assert!(mac.preferred_desktop_artifact("macos-arm64").is_some());
}

#[test]
fn linux_minimal_manifest_corpus_roundtrips() {
    let manifest = load_manifest("linux-minimal.json");
    assert_eq!(manifest.channel, "nightly");
    assert_eq!(manifest.platforms.len(), 1);

    let linux = manifest
        .platforms
        .get("linux-x64")
        .expect("linux-x64 platform missing");
    assert!(linux.preferred_desktop_artifact("linux-x64").is_some());
    assert!(linux.preferred_desktop_artifact("macos-arm64").is_none());
}

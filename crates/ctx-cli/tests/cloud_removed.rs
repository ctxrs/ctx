mod support;

use support::*;

const BUILD_SURFACE_MANIFESTS: [&str; 4] = [
    "Cargo.toml",
    "crates/ctx-cli/Cargo.toml",
    "crates/ctx-cli-presentation/Cargo.toml",
    "crates/ctx-daemon-runtime/Cargo.toml",
];
const REMOVED_BUILD_SURFACES: [&str; 3] = ["ctx-cloud-client", "ctx-captured-batch", "cloud-v2"];

fn removed_build_surface<'a>(
    manifests: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<(&'a str, &'static str)> {
    for (path, manifest) in manifests {
        for removed in REMOVED_BUILD_SURFACES {
            if manifest.contains(removed) {
                return Some((path, removed));
            }
        }
    }
    None
}

#[test]
fn cloud_subcommand_is_not_reachable() {
    let temp = tempdir();
    ctx(&temp)
        .arg("cloud")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand 'cloud'"));
}

#[test]
fn removed_cloud_config_is_rejected_without_initializing_storage() {
    let temp = tempdir();
    let data_root = data_root(&temp);
    fs::create_dir_all(&data_root).unwrap();
    let config_path = data_root.join("config.toml");
    let stale_config = "[cloud]\nmode = \"local_and_cloud\"\n";
    fs::write(&config_path, stale_config).unwrap();

    ctx(&temp)
        .args(["status", "--format=json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "\"code\":\"removed_config_key\",\"config_key\":\"cloud.mode\"",
        ));

    assert_eq!(fs::read_to_string(config_path).unwrap(), stale_config);
    assert!(!data_root.join("search").exists());
    assert!(!data_root.join("relational.sqlite").exists());
    assert!(!data_root.join("spool").exists());
}

#[test]
fn workspace_has_no_cloud_history_targets_or_features() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for removed in [
        "crates/ctx-cloud-client/Cargo.toml",
        "crates/ctx-captured-batch/Cargo.toml",
        "protocol/captured-batch/v2/manifest.json",
    ] {
        assert!(
            !repo_root.join(removed).exists(),
            "{removed} must stay deleted"
        );
    }

    let manifests = BUILD_SURFACE_MANIFESTS.map(|path| {
        (
            path,
            fs::read_to_string(repo_root.join(path))
                .unwrap_or_else(|error| panic!("read {path}: {error}")),
        )
    });
    if let Some((path, removed)) = removed_build_surface(
        manifests
            .iter()
            .map(|(path, manifest)| (*path, manifest.as_str())),
    ) {
        panic!("removed build surface is still reachable from {path}: {removed}");
    }
}

#[test]
fn extracted_cli_manifests_are_fail_closed_cloud_removal_inputs() {
    assert_eq!(
        BUILD_SURFACE_MANIFESTS,
        [
            "Cargo.toml",
            "crates/ctx-cli/Cargo.toml",
            "crates/ctx-cli-presentation/Cargo.toml",
            "crates/ctx-daemon-runtime/Cargo.toml",
        ],
        "cloud-removal policy must retain its exact manifest inventory"
    );
    assert_eq!(
        removed_build_surface([
            ("Cargo.toml", "[workspace]"),
            ("crates/ctx-cli/Cargo.toml", "[package]"),
            (
                "crates/ctx-cli-presentation/Cargo.toml",
                "[dependencies]\nctx-cloud-client = \"1\"",
            ),
            ("crates/ctx-daemon-runtime/Cargo.toml", "[package]"),
        ]),
        Some(("crates/ctx-cli-presentation/Cargo.toml", "ctx-cloud-client"))
    );
}

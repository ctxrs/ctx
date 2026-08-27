use super::*;
use std::{
    thread,
    time::{Duration, Instant},
};

const HELPER_TERMINAL_TIMEOUT: Duration = Duration::from_secs(30);
const HELPER_TERMINAL_POLL: Duration = Duration::from_millis(25);

#[test]
fn semantic_enabled_same_version_upgrade_publishes_legacy_runtime_via_windows_helper() {
    let temp = tempdir();
    let target = fs::canonicalize(bind_test_ctx_binary(&temp)).unwrap();
    unmanaged::install_managed_contract_marker(&target);
    let release = windows_runtime_repair_release(&temp, &target);
    let marker_path = hosted_install_marker_path(&target);
    let binary_before = fs::read(&target).unwrap();
    let marker_before = fs::read(&marker_path).unwrap();

    let scheduled = json_output(
        windows_runtime_repair_release_env(
            ctx_from_binary(&temp, &target).args(["upgrade", "--format=json"]),
            &release,
        )
        .env("CTX_SEARCH_SEMANTIC", "true"),
    );
    assert_eq!(scheduled["status"], "scheduled", "{scheduled:#}");
    assert_eq!(scheduled["applied"], false, "{scheduled:#}");
    assert!(
        scheduled["upgrade_attempt_id"].as_str().is_some(),
        "scheduled helper must identify its durable attempt: {scheduled:#}"
    );

    let journal_path = windows_install_transaction_path(&target);
    let terminal = wait_for_applied_helper_terminal(&journal_path);
    assert_eq!(terminal["phase"], "committed", "{terminal:#}");
    assert_eq!(
        terminal["windows_helper"]["terminal"]["outcome"], "applied",
        "{terminal:#}"
    );

    assert_eq!(fs::read(&target).unwrap(), binary_before);
    assert_eq!(fs::read(&marker_path).unwrap(), marker_before);
    assert_windows_runtime_tree(&release);

    let repeated = json_output(
        windows_runtime_repair_release_env(
            ctx_from_binary(&temp, &target).args(["upgrade", "--format=json"]),
            &release,
        )
        .env("CTX_SEARCH_SEMANTIC", "true"),
    );
    assert_eq!(repeated["status"], "up_to_date", "{repeated:#}");
    assert_eq!(repeated["applied"], false, "{repeated:#}");
    assert!(
        !journal_path.exists(),
        "the second command must reconcile the helper terminal journal"
    );
    assert_eq!(fs::read(&target).unwrap(), binary_before);
    assert_eq!(fs::read(&marker_path).unwrap(), marker_before);
}

fn wait_for_applied_helper_terminal(journal_path: &Path) -> Value {
    let deadline = Instant::now() + HELPER_TERMINAL_TIMEOUT;
    loop {
        match fs::read(journal_path) {
            Ok(bytes) => {
                let journal: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                    panic!(
                        "parse Windows helper journal {} while polling terminal outcome: {error}",
                        journal_path.display()
                    )
                });
                match journal["windows_helper"]["terminal"]["outcome"].as_str() {
                    Some("applied") => return journal,
                    Some("failed") => {
                        panic!("Windows helper reported failed terminal outcome: {journal:#}")
                    }
                    Some(outcome) => {
                        panic!(
                            "unexpected Windows helper terminal outcome {outcome:?}: {journal:#}"
                        )
                    }
                    None => {}
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "read Windows helper journal {} while polling terminal outcome: {error}",
                journal_path.display()
            ),
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Windows helper terminal outcome at {}",
            journal_path.display()
        );
        thread::sleep(HELPER_TERMINAL_POLL);
    }
}

fn assert_windows_runtime_tree(release: &WindowsRuntimeRepairRelease) {
    let lib = release.runtime_target.join("lib");
    for file in [
        "onnxruntime.dll",
        "msvcp140.dll",
        "msvcp140_1.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
    ] {
        assert!(lib.join(file).is_file(), "missing runtime DLL {file}");
    }
    assert_eq!(
        fs::read_to_string(release.runtime_target.join("VERSION_NUMBER")).unwrap(),
        format!("{}\n", release.runtime_version)
    );
    let manifest: Value = serde_json::from_slice(
        &fs::read(release.runtime_target.join("ctx-runtime-install.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["manager"], "ctx-hosted-installer", "{manifest:#}");
    assert_eq!(
        manifest["metadata_trust"], "signed-release-metadata",
        "{manifest:#}"
    );
    assert_eq!(manifest["runtime"], "onnxruntime", "{manifest:#}");
    assert_eq!(manifest["platform"], "windows-x64", "{manifest:#}");
    assert_eq!(manifest["version"], release.runtime_version, "{manifest:#}");
    assert_eq!(
        manifest["sha256"], release.runtime_artifact_sha,
        "{manifest:#}"
    );
}

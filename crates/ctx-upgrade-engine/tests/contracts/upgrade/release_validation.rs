#[cfg(unix)]
use super::*;

#[cfg(unix)]
#[test]
fn upgrade_verifies_signed_metadata_and_fails_closed() {
    let tampered = tempdir();
    let release = fake_release(&tampered, "9.9.9");
    fs::write(
        &release.metadata,
        format!(
            "{}# tampered after signing\n",
            fs::read_to_string(&release.metadata).unwrap()
        ),
    )
    .unwrap();
    let stderr = failure_stderr(fake_release_env(
        ctx(&tampered).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata signature verification failed"),
        "{stderr}"
    );

    let wrong_key = tempdir();
    let release = fake_release(&wrong_key, "9.9.9");
    let stderr = failure_stderr(
        ctx(&wrong_key)
            .args(["upgrade", "check"])
            .env("CTX_UPGRADE_TEST_TARGET", &release.target)
            .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
            .env(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                file_url(&release.signature),
            ),
    );
    assert!(
        stderr.contains("metadata signature verification failed"),
        "{stderr}"
    );

    let bad_signature = tempdir();
    let release = fake_release(&bad_signature, "9.9.9");
    fs::write(&release.signature, "not-base64").unwrap();
    let stderr = failure_stderr(fake_release_env(
        ctx(&bad_signature).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata signature is not base64"),
        "{stderr}"
    );

    let missing_signature = tempdir();
    let release = fake_release(&missing_signature, "9.9.9");
    fs::remove_file(&release.signature).unwrap();
    let stderr = failure_stderr(fake_release_env(
        ctx(&missing_signature).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("download release metadata signature"),
        "{stderr}"
    );

    let default_signature_path = tempdir();
    let release = fake_release(&default_signature_path, "9.9.9");
    let check = json_output(
        ctx(&default_signature_path)
            .args(["upgrade", "check", "--format=json"])
            .env("CTX_UPGRADE_TEST_TARGET", &release.target)
            .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
            .env(
                "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
                TEST_RELEASE_PUBLIC_KEY_PEM,
            ),
    );
    assert_eq!(check["status"], "available");
}

#[cfg(unix)]
#[test]
fn upgrade_rejects_unsafe_metadata_and_bad_artifacts() {
    let duplicate_key = tempdir();
    let release = fake_release(&duplicate_key, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        format!("{metadata}CTX_RELEASE_VERSION=8.8.8\n")
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&duplicate_key).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata contains duplicate key CTX_RELEASE_VERSION"),
        "{stderr}"
    );

    let malformed_bool = tempdir();
    let release = fake_release(&malformed_bool, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            "CTX_RELEASE_SELF_UPGRADE_ALLOWED=true\n",
            "CTX_RELEASE_SELF_UPGRADE_ALLOWED=definitely\n",
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&malformed_bool).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata CTX_RELEASE_SELF_UPGRADE_ALLOWED must be a boolean"),
        "{stderr}"
    );

    let missing_policy = tempdir();
    let release = fake_release(&missing_policy, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata
            .replace("CTX_RELEASE_SELF_UPGRADE_ALLOWED=true\n", "")
            .replace("CTX_RELEASE_AUTO_UPGRADE_ALLOWED=true\n", "")
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&missing_policy).args(["upgrade", "--dry-run"]),
        &release,
    ));
    assert!(stderr.contains("does not allow self-upgrade"), "{stderr}");

    let unsafe_artifact = tempdir();
    let release = fake_release(&unsafe_artifact, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            &format!("CTX_RELEASE_ARTIFACT_{}=ctx\n", test_platform_key()),
            &format!("CTX_RELEASE_ARTIFACT_{}=../ctx\n", test_platform_key()),
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&unsafe_artifact).args(["upgrade", "check"]),
        &release,
    ));
    assert!(stderr.contains("unsafe artifact name"), "{stderr}");

    let unsafe_base = tempdir();
    let release = fake_release(&unsafe_base, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            "CTX_RELEASE_BASE_URL=file://",
            "CTX_RELEASE_BASE_URL=http://",
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&unsafe_base).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata base URL must be HTTPS"),
        "{stderr}"
    );

    let bad_checksum = tempdir();
    let release = fake_release(&bad_checksum, "9.9.9");
    let _runtime = add_fake_release_runtime(&bad_checksum, &release);
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                release.artifact_sha
            ),
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                "f".repeat(64)
            ),
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&bad_checksum).args(["upgrade", "--format=json"]),
        &release,
    ));
    assert!(stderr.contains("artifact checksum mismatch"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn semantic_enabled_same_version_upgrade_repairs_missing_legacy_runtime() {
    let temp = tempdir();
    let release = fake_legacy_release(&temp, env!("CARGO_PKG_VERSION"));
    let runtime = add_fake_release_runtime(&temp, &release);
    let marker_path = install_marker_path(&release.target);
    let cli_before = fs::read(&release.target).unwrap();
    let marker_before = fs::read(&marker_path).unwrap();

    let applied = json_output(
        fake_release_env(ctx(&temp).args(["upgrade", "--format=json"]), &release)
            .env("CTX_SEARCH_SEMANTIC", "true"),
    );

    assert_eq!(applied["status"], "applied");
    assert_eq!(fs::read(&release.target).unwrap(), cli_before);
    assert_eq!(fs::read(&marker_path).unwrap(), marker_before);
    assert_eq!(
        fs::read_to_string(runtime.target.join("VERSION_NUMBER")).unwrap(),
        format!("{}\n", runtime.version)
    );

    let repeated = json_output(
        fake_release_env(ctx(&temp).args(["upgrade", "--format=json"]), &release)
            .env("CTX_SEARCH_SEMANTIC", "true"),
    );
    assert_eq!(repeated["status"], "up_to_date");
    assert_eq!(repeated["applied"], false);
    assert_eq!(fs::read(&release.target).unwrap(), cli_before);
    assert_eq!(fs::read(&marker_path).unwrap(), marker_before);

    let manifest_path = runtime.target.join("ctx-runtime-install.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["sha256"] = json!("f".repeat(64));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let repaired = json_output(
        fake_release_env(ctx(&temp).args(["upgrade", "--format=json"]), &release)
            .env("CTX_SEARCH_SEMANTIC", "true"),
    );
    assert_eq!(repaired["status"], "applied");
    let repaired_manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(repaired_manifest["sha256"], runtime.artifact_sha);
    assert_eq!(fs::read(&release.target).unwrap(), cli_before);
    assert_eq!(fs::read(&marker_path).unwrap(), marker_before);
}

#[cfg(unix)]
#[test]
fn semantic_disabled_same_version_upgrade_does_not_install_legacy_runtime() {
    let temp = tempdir();
    let release = fake_legacy_release(&temp, env!("CARGO_PKG_VERSION"));
    let runtime = add_fake_release_runtime(&temp, &release);

    let outcome = json_output(
        fake_release_env(ctx(&temp).args(["upgrade", "--format=json"]), &release)
            .env("CTX_SEARCH_SEMANTIC", "false"),
    );

    assert_eq!(outcome["status"], "up_to_date");
    assert_eq!(outcome["applied"], false);
    assert!(!runtime.target.exists());
}

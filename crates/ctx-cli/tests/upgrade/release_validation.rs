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

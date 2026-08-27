use super::*;

fn marker_body(install_path: &str, binary_sha256: &str) -> String {
    serde_json::to_string(&json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_path": install_path,
        "platform": platform_key().unwrap(),
        "channel": "stable",
        "version": "1.0.0",
        "sha256": binary_sha256,
    }))
    .unwrap()
}

#[test]
fn windows_hosted_marker_accepts_ordinary_path_for_certified_verbatim_target() {
    let certified = Path::new(r"\\?\C:\Users\ctx\bin\ctx.exe");
    let digest = "a".repeat(64);

    validate_marker_body(
        &marker_body(r"C:\Users\ctx\bin\ctx.exe", &digest),
        certified,
        &digest,
        None,
    )
    .unwrap();
    validate_marker_body(
        &marker_body(r"\\?\C:\Users\ctx\bin\ctx.exe", &digest),
        certified,
        &digest,
        None,
    )
    .unwrap();
}

#[test]
fn windows_hosted_marker_rejects_unsafe_nonlocal_and_aliased_path_claims() {
    let certified = Path::new(r"\\?\C:\Users\ctx\bin\ctx.exe");
    let digest = "a".repeat(64);

    for rejected in [
        r"C:\Users\CTX\bin\ctx.exe",
        r"C:\Users\ctx\other\..\bin\ctx.exe",
        r"\\server\share\ctx.exe",
        r"\\?\UNC\server\share\ctx.exe",
        r"\\.\C:\Users\ctx\bin\ctx.exe",
    ] {
        assert!(
            validate_marker_body(&marker_body(rejected, &digest), certified, &digest, None,)
                .is_err(),
            "accepted unsafe or aliased hosted marker path {rejected}"
        );
    }
}

#[test]
fn windows_hosted_journal_accepts_ordinary_install_and_ownership_marker_paths() {
    let install_path = PathBuf::from(r"\\?\C:\Users\ctx\bin\ctx.exe");
    let ownership_path = ownership_path(&install_path);
    let binary_sha256 = "a".repeat(64);
    let ownership_body = b"owned integrations".to_vec();
    let ownership_sha256 = sha256_hex(&ownership_body);
    let marker_body = serde_json::to_string(&json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_path": r"C:\Users\ctx\bin\ctx.exe",
        "platform": platform_key().unwrap(),
        "channel": "stable",
        "version": "1.0.0",
        "sha256": binary_sha256,
        "integrations_path": r"C:\Users\ctx\bin\ctx.exe.install-integrations",
        "integrations_sha256": ownership_sha256,
    }))
    .unwrap();
    let mut journal = Journal {
        schema_version: SCHEMA_VERSION,
        kind: TransactionKind::Install,
        attempt_id: "ia_12345678".to_owned(),
        marker_path: install_marker_path(&install_path),
        install_path: install_path.clone(),
        binary_sha256,
        marker_sha256: sha256_hex(marker_body.as_bytes()),
        marker_body,
        prior_binary_sha256: None,
        prior_marker_sha256: None,
        prior_ownership_sha256: None,
        ownership_path: Some(ownership_path),
        ownership_sha256: Some(ownership_sha256),
        ownership_body: Some(ownership_body),
        phase: Phase::Prepared,
        binding_sha256: String::new(),
    };
    journal.binding_sha256 = journal_binding(&journal);

    validate_journal(&journal, &install_path, TransactionKind::Install).unwrap();
}

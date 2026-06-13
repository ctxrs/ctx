use super::*;
use ctx_bundled_assets::test_support::{
    bundled_assets_manifest_test_lock, override_bundled_assets_manifest_for_test,
    BundledAssetsManifest,
};
use ctx_bundled_assets::BundledRuntime;

fn current_linux_arch() -> &'static str {
    std::env::consts::ARCH
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn bundled_runtime_entry(os: &str, arch: &str, sha256: String) -> BundledRuntime {
    BundledRuntime {
        id: "ctx-mcp".to_string(),
        version: "0.1.0".to_string(),
        os: os.to_string(),
        arch: arch.to_string(),
        sha256,
        root: format!("runtimes/ctx-mcp/{os}/{arch}"),
        bin: "ctx-mcp".to_string(),
        npm_cli: None,
    }
}

fn write_bundled_mcp(root: &Path, os: &str, arch: &str, contents: &[u8]) -> PathBuf {
    let runtime_root = root.join("runtimes").join("ctx-mcp").join(os).join(arch);
    std::fs::create_dir_all(&runtime_root).expect("mkdir runtime root");
    let bundled_bin = runtime_root.join("ctx-mcp");
    std::fs::write(&bundled_bin, contents).expect("write bundled ctx-mcp");
    bundled_bin
}

#[test]
fn configure_runtime_mcp_command_stages_linux_runtime_for_sandbox_env() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let bundle_root = tempfile::tempdir().expect("bundle root");
    write_bundled_mcp(
        bundle_root.path(),
        "linux",
        current_linux_arch(),
        b"linux ctx-mcp",
    );

    let _bundle_guard = override_bundled_assets_manifest_for_test(
        bundle_root.path().to_path_buf(),
        BundledAssetsManifest {
            version: 1,
            generated_at: None,
            providers: vec![],
            runtimes: vec![bundled_runtime_entry(
                "linux",
                current_linux_arch(),
                sha256_hex(b"linux ctx-mcp"),
            )],
            images: vec![],
        },
    );

    let data_root = tempfile::tempdir().expect("data root");
    let mut provider_env = HashMap::from([(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    )]);

    configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect("configure runtime mcp command");

    let configured = PathBuf::from(
        provider_env
            .get(CTX_MCP_COMMAND_ENV)
            .expect("ctx mcp command should be set"),
    );
    assert!(configured.exists(), "staged ctx-mcp path should exist");
    assert_eq!(
        std::fs::read(&configured).expect("read staged ctx-mcp"),
        b"linux ctx-mcp"
    );
    assert!(configured.starts_with(data_root.path().join("runtimes").join("ctx-mcp")));
}

#[test]
fn configure_runtime_mcp_command_replaces_checksum_mismatched_staged_linux_runtime() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let bundle_root = tempfile::tempdir().expect("bundle root");
    write_bundled_mcp(
        bundle_root.path(),
        "linux",
        current_linux_arch(),
        b"fresh linux ctx-mcp",
    );

    let _bundle_guard = override_bundled_assets_manifest_for_test(
        bundle_root.path().to_path_buf(),
        BundledAssetsManifest {
            version: 1,
            generated_at: None,
            providers: vec![],
            runtimes: vec![bundled_runtime_entry(
                "linux",
                current_linux_arch(),
                sha256_hex(b"fresh linux ctx-mcp"),
            )],
            images: vec![],
        },
    );

    let data_root = tempfile::tempdir().expect("data root");
    let staged = data_root
        .path()
        .join("runtimes")
        .join("ctx-mcp")
        .join("0.1.0")
        .join("ctx-mcp");
    std::fs::create_dir_all(staged.parent().expect("staged parent")).expect("mkdir staged parent");
    std::fs::write(&staged, b"stale linux ctx-mcp").expect("write stale staged runtime");
    let mut provider_env = HashMap::from([(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    )]);

    configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect("configure runtime mcp command");

    assert_eq!(
        std::fs::read(&staged).expect("read staged ctx-mcp"),
        b"fresh linux ctx-mcp"
    );
}

#[test]
fn configure_runtime_mcp_command_fails_closed_with_invalid_linux_runtime_checksum_metadata() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let bundle_root = tempfile::tempdir().expect("bundle root");
    write_bundled_mcp(
        bundle_root.path(),
        "linux",
        current_linux_arch(),
        b"linux ctx-mcp",
    );

    let _bundle_guard = override_bundled_assets_manifest_for_test(
        bundle_root.path().to_path_buf(),
        BundledAssetsManifest {
            version: 1,
            generated_at: None,
            providers: vec![],
            runtimes: vec![bundled_runtime_entry(
                "linux",
                current_linux_arch(),
                "deadbeef".to_string(),
            )],
            images: vec![],
        },
    );

    let data_root = tempfile::tempdir().expect("data root");
    let mut provider_env = HashMap::from([(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    )]);

    let err = configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect_err("invalid checksum metadata should fail closed");
    assert!(err.to_string().contains("valid sha256 metadata"));
}

#[test]
fn configure_runtime_mcp_command_uses_bundled_host_runtime() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let bundle_root = tempfile::tempdir().expect("bundle root");
    let bundled_bin = write_bundled_mcp(
        bundle_root.path(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        b"host ctx-mcp",
    );
    let _bundle_guard = override_bundled_assets_manifest_for_test(
        bundle_root.path().to_path_buf(),
        BundledAssetsManifest {
            version: 1,
            generated_at: None,
            providers: vec![],
            runtimes: vec![bundled_runtime_entry(
                std::env::consts::OS,
                std::env::consts::ARCH,
                sha256_hex(b"host ctx-mcp"),
            )],
            images: vec![],
        },
    );
    let data_root = tempfile::tempdir().expect("data root");
    let mut provider_env = HashMap::new();

    configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect("configure runtime mcp command");

    assert_eq!(
        provider_env.get(CTX_MCP_COMMAND_ENV).map(String::as_str),
        Some(bundled_bin.to_string_lossy().as_ref())
    );
}

#[test]
fn configure_runtime_mcp_command_skips_when_disabled() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let data_root = tempfile::tempdir().expect("data root");
    let mut provider_env = HashMap::from([
        (
            "CTX_HARNESS_CONTAINER_ID".to_string(),
            "ctx-harness-123".to_string(),
        ),
        ("CTX_MCP_DISABLED".to_string(), "1".to_string()),
    ]);

    configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect("disabled mcp should not error");
    assert!(!provider_env.contains_key(CTX_MCP_COMMAND_ENV));
}

#[test]
fn configure_runtime_mcp_command_preserves_existing_explicit_host_command() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let data_root = tempfile::tempdir().expect("data root");
    let explicit = data_root.path().join("ctx-mcp");
    std::fs::write(&explicit, b"host ctx-mcp").expect("write explicit ctx-mcp");
    let mut provider_env = HashMap::from([(
        CTX_MCP_COMMAND_ENV.to_string(),
        explicit.to_string_lossy().to_string(),
    )]);

    configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect("existing ctx mcp command should be preserved");
    assert_eq!(
        provider_env.get(CTX_MCP_COMMAND_ENV).map(String::as_str),
        Some(explicit.to_string_lossy().as_ref())
    );
}

#[test]
fn configure_runtime_mcp_command_rejects_bare_existing_host_command() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let data_root = tempfile::tempdir().expect("data root");
    let mut provider_env =
        HashMap::from([(CTX_MCP_COMMAND_ENV.to_string(), "ctx-mcp".to_string())]);

    let err = configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect_err("bare command should fail closed");
    assert!(err
        .to_string()
        .contains("must be an explicit absolute path"));
}

#[test]
fn configure_runtime_mcp_command_fails_closed_without_host_runtime() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let bundle_root = tempfile::tempdir().expect("bundle root");
    let _bundle_guard = override_bundled_assets_manifest_for_test(
        bundle_root.path().to_path_buf(),
        BundledAssetsManifest {
            version: 1,
            generated_at: None,
            providers: vec![],
            runtimes: vec![],
            images: vec![],
        },
    );
    let data_root = tempfile::tempdir().expect("data root");
    let mut provider_env = HashMap::new();

    let err = configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect_err("missing host runtime should fail");
    assert!(err
        .to_string()
        .contains("host ctx-mcp runtime is unavailable"));
}

#[test]
fn configure_runtime_mcp_command_fails_closed_without_linux_runtime() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let bundle_root = tempfile::tempdir().expect("bundle root");
    let _bundle_guard = override_bundled_assets_manifest_for_test(
        bundle_root.path().to_path_buf(),
        BundledAssetsManifest {
            version: 1,
            generated_at: None,
            providers: vec![],
            runtimes: vec![],
            images: vec![],
        },
    );
    let data_root = tempfile::tempdir().expect("data root");
    let mut provider_env = HashMap::from([(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    )]);

    let err = configure_runtime_mcp_command("codex", &mut provider_env, data_root.path())
        .expect_err("missing runtime should fail");
    assert!(err
        .to_string()
        .contains("linux sandbox ctx-mcp runtime is unavailable"));
}

#[test]
fn configure_runtime_mcp_command_disables_non_mcp_provider_without_runtime() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let bundle_root = tempfile::tempdir().expect("bundle root");
    let _bundle_guard = override_bundled_assets_manifest_for_test(
        bundle_root.path().to_path_buf(),
        BundledAssetsManifest {
            version: 1,
            generated_at: None,
            providers: vec![],
            runtimes: vec![],
            images: vec![],
        },
    );
    let data_root = tempfile::tempdir().expect("data root");
    for provider_id in ["fake", "broken"] {
        let mut provider_env = HashMap::from([(
            CTX_MCP_COMMAND_ENV.to_string(),
            "/should/not/leak".to_string(),
        )]);

        configure_runtime_mcp_command(provider_id, &mut provider_env, data_root.path())
            .expect("non-MCP provider should not require ctx-mcp runtime assets");

        assert_eq!(
            provider_env.get(CTX_MCP_DISABLED_ENV).map(String::as_str),
            Some("1")
        );
        assert!(!provider_env.contains_key(CTX_MCP_COMMAND_ENV));
    }
}

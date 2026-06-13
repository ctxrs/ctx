use super::*;

fn env_lock() -> &'static std::sync::Mutex<()> {
    bundled_assets_manifest_test_lock()
}

#[test]
fn manifest_path_uses_explicit_absolute_override() {
    let _guard = env_lock().lock().expect("bundled assets env lock poisoned");
    let root = PathBuf::from("/tmp/ctx-bundles-root");
    let absolute = PathBuf::from("/tmp/ctx-manifest-absolute.json");
    std::env::set_var(BUNDLE_ENV_MANIFEST, absolute.to_string_lossy().to_string());
    let resolved = manifest_path(&root);
    std::env::remove_var(BUNDLE_ENV_MANIFEST);
    assert_eq!(resolved, absolute);
}

#[test]
fn manifest_path_uses_relative_override_with_bundle_root() {
    let _guard = env_lock().lock().expect("bundled assets env lock poisoned");
    let root = PathBuf::from("/tmp/ctx-bundles-root");
    std::env::set_var(BUNDLE_ENV_MANIFEST, "runtime_manifest.effective.json");
    let resolved = manifest_path(&root);
    std::env::remove_var(BUNDLE_ENV_MANIFEST);
    assert_eq!(resolved, root.join("runtime_manifest.effective.json"));
}

#[test]
fn manifest_path_defaults_to_bundle_manifest() {
    let _guard = env_lock().lock().expect("bundled assets env lock poisoned");
    std::env::remove_var(BUNDLE_ENV_MANIFEST);
    let root = PathBuf::from("/tmp/ctx-bundles-root");
    let resolved = manifest_path(&root);
    assert_eq!(resolved, root.join(MANIFEST_FILENAME));
}

#[test]
fn bundled_runtime_from_manifest_can_select_python_by_version() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let (os, arch) = current_platform();
    let runtime_313_root = root.join("runtimes/python/runtime-313");
    let runtime_312_root = root.join("runtimes/python/runtime-312");
    std::fs::create_dir_all(runtime_313_root.join("bin")).expect("mkdir runtime 313");
    std::fs::create_dir_all(runtime_312_root.join("bin")).expect("mkdir runtime 312");
    std::fs::write(runtime_313_root.join("bin/python3"), b"python313").expect("write runtime 313");
    std::fs::write(runtime_312_root.join("bin/python3"), b"python312").expect("write runtime 312");

    let manifest = BundledAssetsManifest {
        version: MANIFEST_VERSION,
        generated_at: None,
        providers: vec![],
        runtimes: vec![
            BundledRuntime {
                id: "python".to_string(),
                version: "3.13.12".to_string(),
                os: os.to_string(),
                arch: arch.to_string(),
                sha256: "sha313".to_string(),
                root: "runtimes/python/runtime-313".to_string(),
                bin: "bin/python3".to_string(),
                npm_cli: None,
            },
            BundledRuntime {
                id: "python".to_string(),
                version: "3.12.13".to_string(),
                os: os.to_string(),
                arch: arch.to_string(),
                sha256: "sha312".to_string(),
                root: "runtimes/python/runtime-312".to_string(),
                bin: "bin/python3".to_string(),
                npm_cli: None,
            },
        ],
        images: vec![],
    };

    let bundled = bundled_runtime_from_manifest_for_target(
        root,
        &manifest,
        "python",
        Some("3.12.13"),
        os,
        arch,
    )
    .expect("bundled runtime");
    assert_eq!(bundled.version, "3.12.13");
    assert_eq!(bundled.bin, runtime_312_root.join("bin/python3"));
}

#[test]
fn bundled_runtime_for_can_select_explicit_linux_target() {
    let _guard = bundled_assets_manifest_test_lock()
        .lock()
        .expect("bundled assets manifest lock poisoned");
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let runtime_root = root.join("runtimes/ctx-mcp/linux/aarch64");
    std::fs::create_dir_all(&runtime_root).expect("mkdir runtime root");
    std::fs::write(runtime_root.join("ctx-mcp"), b"linux ctx-mcp").expect("write runtime");
    let _guard = override_bundled_assets_manifest_for_test(
        root.to_path_buf(),
        BundledAssetsManifest {
            version: MANIFEST_VERSION,
            generated_at: None,
            providers: vec![],
            runtimes: vec![BundledRuntime {
                id: "ctx-mcp".to_string(),
                version: "0.1.0".to_string(),
                os: "linux".to_string(),
                arch: "aarch64".to_string(),
                sha256: "sha".to_string(),
                root: "runtimes/ctx-mcp/linux/aarch64".to_string(),
                bin: "ctx-mcp".to_string(),
                npm_cli: None,
            }],
            images: vec![],
        },
    );

    let bundled = bundled_runtime_for("ctx-mcp", "linux", "aarch64").expect("bundled runtime");
    assert_eq!(bundled.version, "0.1.0");
    assert_eq!(bundled.sha256, "sha");
    assert_eq!(bundled.bin, runtime_root.join("ctx-mcp"));
}

#[test]
fn select_managed_source_ignores_local_entries() {
    let component = RuntimeLockComponent {
        kind: "image".to_string(),
        id: "ctx-harness".to_string(),
        os: "linux".to_string(),
        arch: "aarch64".to_string(),
        variant: Some("default".to_string()),
        version: None,
        bin: None,
        helpers: HashMap::new(),
        sources: vec![
            RuntimeLockSource {
                source_type: "local".to_string(),
                uri: None,
                sha256: None,
            },
            RuntimeLockSource {
                source_type: "ci".to_string(),
                uri: Some("https://example.test/image.tar".to_string()),
                sha256: Some("abcd".to_string()),
            },
        ],
    };
    let mut allowed = HashSet::new();
    allowed.insert("ci".to_string());
    let source = select_managed_source(&component, &allowed).expect("managed source");
    assert_eq!(source.uri, "https://example.test/image.tar");
    assert_eq!(source.sha256, "abcd");
}

#[test]
fn select_managed_source_respects_allowed_source_types() {
    let component = RuntimeLockComponent {
        kind: "image".to_string(),
        id: "ctx-harness".to_string(),
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
        variant: Some("default".to_string()),
        version: None,
        bin: None,
        helpers: HashMap::new(),
        sources: vec![RuntimeLockSource {
            source_type: "vendor".to_string(),
            uri: Some("https://example.test/image.tar".to_string()),
            sha256: Some("abcd".to_string()),
        }],
    };
    let mut allowed = HashSet::new();
    allowed.insert("ci".to_string());
    assert!(select_managed_source(&component, &allowed).is_none());
}

#[test]
fn select_managed_runtime_source_extracts_version_bin_and_helpers() {
    let component = RuntimeLockComponent {
        kind: "runtime".to_string(),
        id: "sandbox-cli".to_string(),
        os: "macos".to_string(),
        arch: "aarch64".to_string(),
        variant: Some("default".to_string()),
        version: Some("5.8.0".to_string()),
        bin: Some("usr/bin/nerdctl".to_string()),
        helpers: HashMap::from([(
            "gvproxy".to_string(),
            RuntimeLockHelperSource {
                uri: Some("https://example.test/gvproxy".to_string()),
                sha256: Some("1234".to_string()),
            },
        )]),
        sources: vec![RuntimeLockSource {
            source_type: "vendor".to_string(),
            uri: Some("https://example.test/sandbox-cli.tgz".to_string()),
            sha256: Some("abcd".to_string()),
        }],
    };
    let mut allowed = HashSet::new();
    allowed.insert("vendor".to_string());
    let source = select_managed_runtime_source(&component, &allowed).expect("runtime source");
    assert_eq!(source.uri, "https://example.test/sandbox-cli.tgz");
    assert_eq!(source.sha256, "abcd");
    assert_eq!(source.version, "5.8.0");
    assert_eq!(source.bin, "usr/bin/nerdctl");
    let helper = source
        .helpers
        .get("gvproxy")
        .expect("gvproxy helper should be present");
    assert_eq!(helper.uri, "https://example.test/gvproxy");
    assert_eq!(helper.sha256, "1234");
}

#[test]
fn select_managed_runtime_source_supports_avf_guest_helper_payloads() {
    let component = RuntimeLockComponent {
        kind: "runtime".to_string(),
        id: "avf-linux-guest".to_string(),
        os: "macos".to_string(),
        arch: "aarch64".to_string(),
        variant: Some("default".to_string()),
        version: Some("ubuntu-noble-arm64-test".to_string()),
        bin: Some("rootfs.raw".to_string()),
        helpers: HashMap::from([
            (
                "kernel".to_string(),
                RuntimeLockHelperSource {
                    uri: Some("https://example.test/kernel".to_string()),
                    sha256: Some("1111".to_string()),
                },
            ),
            (
                "initrd".to_string(),
                RuntimeLockHelperSource {
                    uri: Some("https://example.test/initrd".to_string()),
                    sha256: Some("2222".to_string()),
                },
            ),
            (
                "egress-proxy".to_string(),
                RuntimeLockHelperSource {
                    uri: Some("https://example.test/egress-proxy".to_string()),
                    sha256: Some("3333".to_string()),
                },
            ),
            (
                "guest-agent".to_string(),
                RuntimeLockHelperSource {
                    uri: Some("https://example.test/guest-agent".to_string()),
                    sha256: Some("5555".to_string()),
                },
            ),
            (
                "container-stack".to_string(),
                RuntimeLockHelperSource {
                    uri: Some("https://example.test/container-stack".to_string()),
                    sha256: Some("4444".to_string()),
                },
            ),
        ]),
        sources: vec![RuntimeLockSource {
            source_type: "ci".to_string(),
            uri: Some("https://example.test/rootfs.raw.zst".to_string()),
            sha256: Some("abcd".to_string()),
        }],
    };
    let mut allowed = HashSet::new();
    allowed.insert("ci".to_string());
    let source = select_managed_runtime_source(&component, &allowed).expect("runtime source");
    assert_eq!(source.uri, "https://example.test/rootfs.raw.zst");
    assert_eq!(source.sha256, "abcd");
    assert_eq!(source.version, "ubuntu-noble-arm64-test");
    assert_eq!(source.bin, "rootfs.raw");
    assert_eq!(
        source
            .helpers
            .get("kernel")
            .map(|helper| helper.uri.as_str()),
        Some("https://example.test/kernel")
    );
    assert_eq!(
        source
            .helpers
            .get("initrd")
            .map(|helper| helper.uri.as_str()),
        Some("https://example.test/initrd")
    );
    assert_eq!(
        source
            .helpers
            .get("egress-proxy")
            .map(|helper| helper.uri.as_str()),
        Some("https://example.test/egress-proxy")
    );
    assert_eq!(
        source
            .helpers
            .get("guest-agent")
            .map(|helper| helper.uri.as_str()),
        Some("https://example.test/guest-agent")
    );
    assert_eq!(
        source
            .helpers
            .get("container-stack")
            .map(|helper| helper.uri.as_str()),
        Some("https://example.test/container-stack")
    );
}

#[test]
fn select_managed_runtime_source_rejects_unresolved_avf_placeholder_payloads() {
    let component = RuntimeLockComponent {
        kind: "runtime".to_string(),
        id: "avf-linux-guest".to_string(),
        os: "macos".to_string(),
        arch: "aarch64".to_string(),
        variant: Some("default".to_string()),
        version: Some("locked".to_string()),
        bin: Some("rootfs.raw".to_string()),
        helpers: HashMap::from([
            (
                "kernel".to_string(),
                RuntimeLockHelperSource {
                    uri: Some("locked://runtimes/avf-linux-guest/macos/aarch64/kernel".to_string()),
                    sha256: Some("0".repeat(64)),
                },
            ),
            (
                "initrd".to_string(),
                RuntimeLockHelperSource {
                    uri: Some("locked://runtimes/avf-linux-guest/macos/aarch64/initrd".to_string()),
                    sha256: Some("0".repeat(64)),
                },
            ),
            (
                "guest-agent".to_string(),
                RuntimeLockHelperSource {
                    uri: Some(
                        "locked://runtimes/avf-linux-guest/macos/aarch64/guest-agent".to_string(),
                    ),
                    sha256: Some("0".repeat(64)),
                },
            ),
            (
                "egress-proxy".to_string(),
                RuntimeLockHelperSource {
                    uri: Some(
                        "locked://runtimes/avf-linux-guest/macos/aarch64/egress-proxy".to_string(),
                    ),
                    sha256: Some("0".repeat(64)),
                },
            ),
            (
                "container-stack".to_string(),
                RuntimeLockHelperSource {
                    uri: Some(
                        "locked://runtimes/avf-linux-guest/macos/aarch64/container-stack"
                            .to_string(),
                    ),
                    sha256: Some("0".repeat(64)),
                },
            ),
        ]),
        sources: vec![RuntimeLockSource {
            source_type: "ci".to_string(),
            uri: Some("locked://runtimes/avf-linux-guest/macos/aarch64/rootfs.raw.zst".to_string()),
            sha256: Some("0".repeat(64)),
        }],
    };
    let mut allowed = HashSet::new();
    allowed.insert("ci".to_string());

    assert!(select_managed_runtime_source(&component, &allowed).is_none());
}

#[test]
fn managed_sandbox_machine_cache_source_can_be_overridden_for_tests() {
    let override_source = ManagedArtifactSource {
        uri: "https://example.test/sandbox-machine.raw.zst".to_string(),
        sha256: "cafebabe".to_string(),
    };
    let guard = override_managed_sandbox_machine_cache_source_for_test(override_source.clone());
    let resolved = managed_sandbox_machine_cache_source().expect("override should resolve");
    assert_eq!(resolved.uri, override_source.uri);
    assert_eq!(resolved.sha256, override_source.sha256);
    drop(guard);
    if let Some(restored) = managed_sandbox_machine_cache_source() {
        assert!(
            restored.uri != override_source.uri || restored.sha256 != override_source.sha256,
            "dropping the guard should restore the prior source"
        );
    }
}

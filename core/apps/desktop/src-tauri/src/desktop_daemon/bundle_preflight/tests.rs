use super::*;
use std::fs;

#[test]
fn parse_target_requires_os_arch_pair() {
    assert_eq!(
        parse_target("linux/x86_64", "macos", "aarch64"),
        Some(RuntimeTarget {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        })
    );
    assert_eq!(parse_target("linux", "macos", "aarch64"), None);
    assert_eq!(parse_target("", "macos", "aarch64"), None);
    assert_eq!(parse_target("linux/", "macos", "aarch64"), None);
}

#[test]
fn parse_target_normalizes_host_tokens() {
    assert_eq!(
        parse_target("host/host", "macos", "aarch64"),
        Some(RuntimeTarget {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        })
    );
}

#[test]
fn required_targets_falls_back_when_configured_is_empty_or_invalid() {
    let fallback = vec![RuntimeTarget {
        os: "macos".to_string(),
        arch: "aarch64".to_string(),
    }];
    assert_eq!(
        required_targets_or_default(&[], &fallback, "macos", "aarch64"),
        fallback
    );
    assert_eq!(
        required_targets_or_default(&["invalid".to_string()], &fallback, "macos", "aarch64"),
        fallback
    );
}

#[test]
fn required_targets_normalize_host_and_preserve_concrete_targets() {
    let fallback = vec![
        RuntimeTarget {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        RuntimeTarget {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        },
    ];
    assert_eq!(
        required_targets_or_default(
            &["host/host".to_string(), "linux/x86_64".to_string()],
            &fallback,
            "macos",
            "aarch64",
        ),
        fallback
    );
}

#[test]
fn host_relevant_targets_filters_to_allowed_subset() {
    let configured = vec![
        RuntimeTarget {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        RuntimeTarget {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        },
    ];
    let fallback = vec![RuntimeTarget {
        os: "linux".to_string(),
        arch: "x86_64".to_string(),
    }];
    assert_eq!(host_relevant_targets(&configured, &fallback), fallback);
}

#[test]
fn host_relevant_targets_uses_fallback_when_none_match() {
    let configured = vec![RuntimeTarget {
        os: "windows".to_string(),
        arch: "x86_64".to_string(),
    }];
    let fallback = vec![RuntimeTarget {
        os: "linux".to_string(),
        arch: "aarch64".to_string(),
    }];
    assert!(host_relevant_targets(&configured, &fallback).is_empty());
}

#[test]
fn explicit_runtime_targets_do_not_fall_back_to_unconfigured_host_defaults() {
    let fallback = vec![
        RuntimeTarget {
            os: "macos".to_string(),
            arch: "x86_64".to_string(),
        },
        RuntimeTarget {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        },
    ];
    let configured =
        required_targets_or_default(&["macos/aarch64".to_string()], &fallback, "macos", "x86_64");

    assert_eq!(
        configured,
        vec![RuntimeTarget {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        }]
    );
    assert!(host_relevant_targets(&configured, &fallback).is_empty());
}

#[test]
fn thin_bundle_runtime_requirement_accepts_managed_runtime_source() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix epoch")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "ctx-desktop-bundle-preflight-{}-{}",
        std::process::id(),
        nonce
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    fs::write(
        temp.join("manifest.json"),
        r#"{
  "version": 1,
  "providers": [],
  "runtimes": [],
  "images": []
}"#,
    )
    .expect("write manifest");
    fs::write(
        temp.join("runtime_lock.v2.json"),
        format!(
            r#"{{
  "version": 2,
  "profiles": {{
    "parity": {{
      "allowed_source_types": ["ci", "vendor"]
    }}
  }},
  "required": {{
    "targets": {{
      "provider": [],
      "runtime": ["host/host"],
      "image": [],
      "machine_cache": []
    }},
    "provider_ids": [],
    "runtime_ids": ["node"],
    "image_ids": [],
    "machine_cache_ids": []
  }},
  "components": [
    {{
      "kind": "runtime",
      "id": "node",
      "os": "{os}",
      "arch": "{arch}",
      "variant": "default",
      "sources": [
        {{
          "source_type": "ci",
          "uri": "locked://runtime/node/{os}/{arch}",
          "sha256": "{sha}"
        }}
      ]
    }}
  ]
}}"#,
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            sha = "1".repeat(64),
        ),
    )
    .expect("write runtime lock");

    enforce_desktop_parity_bundle_preflight(Some(&temp))
        .expect("managed runtime source should satisfy thin-bundle preflight");
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn thin_bundle_avf_runtime_requires_helper_metadata() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix epoch")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "ctx-desktop-bundle-preflight-avf-lock-{}-{}",
        std::process::id(),
        nonce
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    fs::write(
        temp.join("manifest.json"),
        r#"{
  "version": 1,
  "providers": [],
  "runtimes": [],
  "images": []
}"#,
    )
    .expect("write manifest");
    fs::write(
        temp.join("runtime_lock.v2.json"),
        format!(
            r#"{{
  "version": 2,
  "profiles": {{
    "parity": {{
      "allowed_source_types": ["ci", "vendor"]
    }}
  }},
  "required": {{
    "targets": {{
      "provider": [],
      "runtime": ["macos/host"],
      "image": [],
      "machine_cache": []
    }},
    "provider_ids": [],
    "runtime_ids": ["avf-linux-guest"],
    "image_ids": [],
    "machine_cache_ids": []
  }},
  "components": [
    {{
      "kind": "runtime",
      "id": "avf-linux-guest",
      "os": "{os}",
      "arch": "{arch}",
      "variant": "default",
      "version": "locked",
      "sources": [
        {{
          "source_type": "ci",
          "uri": "https://example.invalid/runtime/avf-linux-guest/{os}/{arch}.tar.zst",
          "sha256": "{sha}"
        }}
      ]
    }}
  ]
}}"#,
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            sha = "1".repeat(64),
        ),
    )
    .expect("write runtime lock");

    let err = enforce_desktop_parity_bundle_preflight(Some(&temp))
        .expect_err("missing helper metadata should fail");
    assert!(err.to_string().contains("AVF helper metadata"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn bundled_avf_runtime_requires_helper_payloads() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix epoch")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "ctx-desktop-bundle-preflight-avf-bundle-{}-{}",
        std::process::id(),
        nonce
    ));
    let host_os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    let host_arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        std::env::consts::ARCH
    };
    let runtime_root = temp
        .join("runtimes")
        .join("avf-linux-guest")
        .join(host_os)
        .join(host_arch);
    let helpers_dir = runtime_root.join("helpers");
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&helpers_dir).expect("create helpers dir");
    fs::write(runtime_root.join("rootfs.raw"), "rootfs").expect("write rootfs");
    fs::write(helpers_dir.join("kernel"), "kernel").expect("write kernel");
    fs::write(helpers_dir.join("initrd"), "initrd").expect("write initrd");
    fs::write(helpers_dir.join("egress-proxy"), "proxy").expect("write proxy");
    fs::write(
        helpers_dir.join("container-stack.tar.gz"),
        "container-stack",
    )
    .expect("write container stack");
    fs::write(
        temp.join("manifest.json"),
        format!(
            r#"{{
  "version": 1,
  "providers": [],
  "runtimes": [
    {{
      "id": "avf-linux-guest",
      "os": "{os}",
      "arch": "{arch}",
      "root": "runtimes/avf-linux-guest/{os}/{arch}",
      "bin": "rootfs.raw"
    }}
  ],
  "images": []
}}"#,
            os = host_os,
            arch = host_arch,
        ),
    )
    .expect("write manifest");
    fs::write(
        temp.join("runtime_lock.v2.json"),
        format!(
            r#"{{
  "version": 2,
  "profiles": {{
    "parity": {{
      "allowed_source_types": ["ci", "vendor"]
    }}
  }},
  "required": {{
    "targets": {{
      "provider": [],
      "runtime": ["{os}/host"],
      "image": [],
      "machine_cache": []
    }},
    "provider_ids": [],
    "runtime_ids": ["avf-linux-guest"],
    "image_ids": [],
    "machine_cache_ids": []
  }},
  "components": [
    {{
      "kind": "runtime",
      "id": "avf-linux-guest",
      "os": "{os}",
      "arch": "{arch}",
      "variant": "default",
      "version": "locked",
      "sources": [
        {{
          "source_type": "ci",
          "uri": "https://example.invalid/runtime/avf-linux-guest/{os}/{arch}",
          "sha256": "{sha}"
        }}
      ],
      "helpers": {{
        "kernel": {{ "uri": "locked://kernel", "sha256": "{sha}" }},
        "initrd": {{ "uri": "locked://initrd", "sha256": "{sha}" }},
        "guest-agent": {{ "uri": "locked://guest-agent", "sha256": "{sha}" }},
        "egress-proxy": {{ "uri": "locked://egress-proxy", "sha256": "{sha}" }},
        "container-stack": {{ "uri": "locked://container-stack", "sha256": "{sha}" }}
      }}
    }}
  ]
}}"#,
            os = host_os,
            arch = host_arch,
            sha = "1".repeat(64),
        ),
    )
    .expect("write runtime lock");

    let err = enforce_desktop_parity_bundle_preflight(Some(&temp))
        .expect_err("missing guest-agent helper should fail");
    assert!(err.to_string().contains("guest-agent"));
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

#[test]
fn thin_bundle_avf_runtime_entry_rejects_unresolved_managed_runtime_source_without_bundled_root() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix epoch")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "ctx-desktop-bundle-preflight-avf-thin-{}-{}",
        std::process::id(),
        nonce
    ));
    let host_os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    let host_arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        std::env::consts::ARCH
    };
    if temp.exists() {
        fs::remove_dir_all(&temp).expect("clear tempdir");
    }
    fs::create_dir_all(&temp).expect("create tempdir");
    fs::write(
        temp.join("manifest.json"),
        format!(
            r#"{{
  "version": 1,
  "providers": [],
  "runtimes": [
    {{
      "id": "avf-linux-guest",
      "os": "{os}",
      "arch": "{arch}",
      "root": "runtimes/avf-linux-guest/{os}/{arch}/ubuntu-noble-arm64-0ade3ab45292",
      "bin": "rootfs.raw"
    }}
  ],
  "images": []
}}"#,
            os = host_os,
            arch = host_arch,
        ),
    )
    .expect("write manifest");
    fs::write(
        temp.join("runtime_lock.v2.json"),
        format!(
            r#"{{
  "version": 2,
  "profiles": {{
    "parity": {{
      "allowed_source_types": ["ci", "vendor"]
    }}
  }},
  "required": {{
    "targets": {{
      "provider": [],
      "runtime": ["{os}/host"],
      "image": [],
      "machine_cache": []
    }},
    "provider_ids": [],
    "runtime_ids": ["avf-linux-guest"],
    "image_ids": [],
    "machine_cache_ids": []
  }},
  "components": [
    {{
      "kind": "runtime",
      "id": "avf-linux-guest",
      "os": "{os}",
      "arch": "{arch}",
      "variant": "default",
      "version": "locked",
      "sources": [
        {{
          "source_type": "ci",
          "uri": "locked://runtime/avf-linux-guest/{os}/{arch}",
          "sha256": "{sha}"
        }}
      ],
      "helpers": {{
        "kernel": {{ "uri": "locked://kernel", "sha256": "{sha}" }},
        "initrd": {{ "uri": "locked://initrd", "sha256": "{sha}" }},
        "guest-agent": {{ "uri": "locked://guest-agent", "sha256": "{sha}" }},
        "egress-proxy": {{ "uri": "locked://egress-proxy", "sha256": "{sha}" }},
        "container-stack": {{ "uri": "locked://container-stack", "sha256": "{sha}" }}
      }}
    }}
  ]
}}"#,
            os = host_os,
            arch = host_arch,
            sha = "0".repeat(64),
        ),
    )
    .expect("write runtime lock");

    let err = enforce_desktop_parity_bundle_preflight(Some(&temp))
        .expect_err("placeholder AVF managed source should not satisfy thin-bundle preflight");
    let rendered = err.to_string();
    assert!(
        rendered.contains("runtime lock missing AVF helper metadata")
            || rendered.contains("missing runtime root dir"),
        "expected unresolved AVF managed source failure, got: {rendered}"
    );
    fs::remove_dir_all(&temp).expect("cleanup tempdir");
}

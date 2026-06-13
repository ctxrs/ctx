use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeTarget {
    pub(crate) os: String,
    pub(crate) arch: String,
}

fn normalize_target_token(raw: &str, host_value: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.eq_ignore_ascii_case("host") {
        return Some(host_value.to_string());
    }
    Some(trimmed.to_string())
}

pub(crate) fn parse_target(raw: &str, host_os: &str, host_arch: &str) -> Option<RuntimeTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (os, arch) = trimmed.split_once('/')?;
    let os = normalize_target_token(os, host_os)?;
    let arch = normalize_target_token(arch, host_arch)?;
    Some(RuntimeTarget { os, arch })
}

pub(crate) fn required_targets_or_default(
    configured: &[String],
    fallback: &[RuntimeTarget],
    host_os: &str,
    host_arch: &str,
) -> Vec<RuntimeTarget> {
    if configured.is_empty() {
        return fallback.to_vec();
    }
    let mut out = Vec::<RuntimeTarget>::new();
    for value in configured {
        if let Some(target) = parse_target(value, host_os, host_arch) {
            if !out.contains(&target) {
                out.push(target);
            }
        }
    }
    if out.is_empty() {
        return fallback.to_vec();
    }
    out
}

pub(crate) fn host_default_provider_targets() -> Vec<RuntimeTarget> {
    let host_os = std::env::consts::OS.to_string();
    let host_arch = std::env::consts::ARCH.to_string();
    if host_os == "macos" && host_arch == "aarch64" {
        return vec![
            RuntimeTarget {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
            RuntimeTarget {
                os: "linux".to_string(),
                arch: "aarch64".to_string(),
            },
            RuntimeTarget {
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
        ];
    }
    let mut out = vec![RuntimeTarget {
        os: host_os.clone(),
        arch: host_arch.clone(),
    }];
    let linux_target = RuntimeTarget {
        os: "linux".to_string(),
        arch: host_arch,
    };
    if !out.contains(&linux_target) {
        out.push(linux_target);
    }
    out
}

pub(crate) fn host_default_runtime_targets() -> Vec<RuntimeTarget> {
    host_default_provider_targets()
}

pub(crate) fn host_default_image_targets() -> Vec<RuntimeTarget> {
    let host_arch = std::env::consts::ARCH.to_string();
    if std::env::consts::OS == "macos" && host_arch == "aarch64" {
        return vec![
            RuntimeTarget {
                os: "linux".to_string(),
                arch: "aarch64".to_string(),
            },
            RuntimeTarget {
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            },
        ];
    }
    vec![RuntimeTarget {
        os: "linux".to_string(),
        arch: host_arch,
    }]
}

pub(crate) fn host_default_machine_cache_targets() -> Vec<RuntimeTarget> {
    if std::env::consts::OS != "macos" {
        return Vec::new();
    }
    vec![RuntimeTarget {
        os: "macos".to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }]
}

pub(crate) fn host_relevant_targets(
    all_targets: &[RuntimeTarget],
    fallback: &[RuntimeTarget],
) -> Vec<RuntimeTarget> {
    if all_targets.is_empty() {
        return fallback.to_vec();
    }
    let mut out = Vec::<RuntimeTarget>::new();
    for target in all_targets {
        if fallback.contains(target) && !out.contains(target) {
            out.push(target.clone());
        }
    }
    out
}

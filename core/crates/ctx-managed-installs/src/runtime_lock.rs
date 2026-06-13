use super::*;

pub(crate) const RUNTIME_READY_METADATA_FILENAME: &str = ".ctx-runtime-ready.json";

const MIRROR_HOST: &str = "api.ctx.rs";
const MIRROR_PATH_PREFIX: &str = "/storage/v1/object/public/releases/artifacts/managed-runtimes/";
const READY_METADATA_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedRuntimeKind {
    Node,
    Python,
}

impl ManagedRuntimeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Python => "python",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedRuntimeArchiveKind {
    TarGz,
    Zip,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ManagedRuntimeArchiveSpec {
    pub(crate) kind: ManagedRuntimeKind,
    pub(crate) version: &'static str,
    pub(crate) build_tag: Option<&'static str>,
    pub(crate) target: &'static str,
    pub(crate) archive_name: &'static str,
    pub(crate) archive_kind: ManagedRuntimeArchiveKind,
    pub(crate) mirror_url: &'static str,
    pub(crate) sha256: &'static str,
}

impl ManagedRuntimeArchiveSpec {
    pub(crate) fn sha256_prefix(self) -> &'static str {
        self.sha256.get(..12).unwrap_or(self.sha256)
    }

    pub(crate) fn expected_extract_root(self) -> String {
        match self.kind {
            ManagedRuntimeKind::Node => format!("node-v{}-{}", self.version, self.target),
            ManagedRuntimeKind::Python => format!(
                "cpython-{}+{}-{}",
                self.version,
                self.build_tag.unwrap_or_default(),
                self.target
            ),
        }
    }

    pub(crate) fn content_scoped_install_dir_name(self) -> String {
        format!(
            "{}-sha256-{}",
            self.expected_extract_root(),
            self.sha256_prefix()
        )
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct RuntimeReadyMetadata {
    schema_version: u32,
    kind: ManagedRuntimeKind,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_tag: Option<String>,
    target: String,
    archive_name: String,
    mirror_url: String,
    sha256: String,
    installed_at: String,
}

impl RuntimeReadyMetadata {
    fn from_spec(spec: &ManagedRuntimeArchiveSpec) -> Self {
        Self {
            schema_version: READY_METADATA_SCHEMA_VERSION,
            kind: spec.kind,
            version: spec.version.to_string(),
            build_tag: spec.build_tag.map(ToOwned::to_owned),
            target: spec.target.to_string(),
            archive_name: spec.archive_name.to_string(),
            mirror_url: spec.mirror_url.to_string(),
            sha256: spec.sha256.to_ascii_lowercase(),
            installed_at: Utc::now().to_rfc3339(),
        }
    }

    fn matches_spec(&self, spec: &ManagedRuntimeArchiveSpec) -> bool {
        self.schema_version == READY_METADATA_SCHEMA_VERSION
            && self.kind == spec.kind
            && self.version == spec.version
            && self.build_tag.as_deref() == spec.build_tag
            && self.target == spec.target
            && self.archive_name == spec.archive_name
            && self.mirror_url == spec.mirror_url
            && self.sha256.eq_ignore_ascii_case(spec.sha256)
    }
}

const NODE_RUNTIME_ARCHIVES: &[ManagedRuntimeArchiveSpec] = &[
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Node,
        version: "24.15.0",
        build_tag: None,
        target: "darwin-arm64",
        archive_name: "node-v24.15.0-darwin-arm64.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-darwin-arm64.tar.gz",
        sha256: "372331b969779ab5d15b949884fc6eaf88d5afe87bde8ba881d6400b9100ffc4",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Node,
        version: "24.15.0",
        build_tag: None,
        target: "darwin-x64",
        archive_name: "node-v24.15.0-darwin-x64.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-darwin-x64.tar.gz",
        sha256: "ffd5ee293467927f3ee731a553eb88fd1f48cf74eebc2d74a6babe4af228673b",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Node,
        version: "24.15.0",
        build_tag: None,
        target: "linux-arm64",
        archive_name: "node-v24.15.0-linux-arm64.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-linux-arm64.tar.gz",
        sha256: "73afc234d558c24919875f51c2d1ea002a2ada4ea6f83601a383869fefa64eed",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Node,
        version: "24.15.0",
        build_tag: None,
        target: "linux-x64",
        archive_name: "node-v24.15.0-linux-x64.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-linux-x64.tar.gz",
        sha256: "44836872d9aec49f1e6b52a9a922872db9a2b02d235a616a5681b6a85fec8d89",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Node,
        version: "24.15.0",
        build_tag: None,
        target: "win-arm64",
        archive_name: "node-v24.15.0-win-arm64.zip",
        archive_kind: ManagedRuntimeArchiveKind::Zip,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-win-arm64.zip",
        sha256: "c9eb7402eda26e2ba7e44b6727fc85a8de56c5095b1f71ebd3062892211aa116",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Node,
        version: "24.15.0",
        build_tag: None,
        target: "win-x64",
        archive_name: "node-v24.15.0-win-x64.zip",
        archive_kind: ManagedRuntimeArchiveKind::Zip,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-win-x64.zip",
        sha256: "cc5149eabd53779ce1e7bdc5401643622d0c7e6800ade18928a767e940bb0e62",
    },
];

const PYTHON_RUNTIME_ARCHIVES: &[ManagedRuntimeArchiveSpec] = &[
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.13.13",
        build_tag: Some("20260414"),
        target: "aarch64-apple-darwin",
        archive_name: "cpython-3.13.13+20260414-aarch64-apple-darwin-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.13.13/20260414/cpython-3.13.13+20260414-aarch64-apple-darwin-install_only.tar.gz",
        sha256: "c652dad552122cd2e76968ec41c803f8222038169b11310dba0c85928265f5c1",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.13.13",
        build_tag: Some("20260414"),
        target: "x86_64-apple-darwin",
        archive_name: "cpython-3.13.13+20260414-x86_64-apple-darwin-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.13.13/20260414/cpython-3.13.13+20260414-x86_64-apple-darwin-install_only.tar.gz",
        sha256: "540337412d2c4220e99280f741dbf45c1e3da3a39edaaab20c6ba1d53e1692ef",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.13.13",
        build_tag: Some("20260414"),
        target: "aarch64-unknown-linux-gnu",
        archive_name: "cpython-3.13.13+20260414-aarch64-unknown-linux-gnu-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.13.13/20260414/cpython-3.13.13+20260414-aarch64-unknown-linux-gnu-install_only.tar.gz",
        sha256: "6a65f68043d7fadcd580415493d2929d1fd686013f9ae44ddbd3a81307ab256d",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.13.13",
        build_tag: Some("20260414"),
        target: "x86_64-unknown-linux-gnu",
        archive_name: "cpython-3.13.13+20260414-x86_64-unknown-linux-gnu-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.13.13/20260414/cpython-3.13.13+20260414-x86_64-unknown-linux-gnu-install_only.tar.gz",
        sha256: "e5ec3b2c5693215d153c434ac018e75511b2c4f96d2bce30468a477cb3a89d5e",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.13.13",
        build_tag: Some("20260414"),
        target: "aarch64-pc-windows-msvc",
        archive_name: "cpython-3.13.13+20260414-aarch64-pc-windows-msvc-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.13.13/20260414/cpython-3.13.13+20260414-aarch64-pc-windows-msvc-install_only.tar.gz",
        sha256: "586ba71c75f341e1d111399b7f719ae784dc11e8672e93e017388f28684226d0",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.13.13",
        build_tag: Some("20260414"),
        target: "x86_64-pc-windows-msvc",
        archive_name: "cpython-3.13.13+20260414-x86_64-pc-windows-msvc-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.13.13/20260414/cpython-3.13.13+20260414-x86_64-pc-windows-msvc-install_only.tar.gz",
        sha256: "ee0cb26453d6e025d36502d765c1639c34830355e46ab3ad31c0360bc4cd9b79",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.12.13",
        build_tag: Some("20260303"),
        target: "aarch64-apple-darwin",
        archive_name: "cpython-3.12.13+20260303-aarch64-apple-darwin-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.12.13/20260303/cpython-3.12.13+20260303-aarch64-apple-darwin-install_only.tar.gz",
        sha256: "2c01f29e9e4ddbd57e0319fedecf1f3e222558fce394a3ed4e39d0f750c11988",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.12.13",
        build_tag: Some("20260303"),
        target: "x86_64-apple-darwin",
        archive_name: "cpython-3.12.13+20260303-x86_64-apple-darwin-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.12.13/20260303/cpython-3.12.13+20260303-x86_64-apple-darwin-install_only.tar.gz",
        sha256: "8ad52a15de26e67d53f5c14f338433c59d5a2711852adc59043a20ec8da71a52",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.12.13",
        build_tag: Some("20260303"),
        target: "aarch64-unknown-linux-gnu",
        archive_name: "cpython-3.12.13+20260303-aarch64-unknown-linux-gnu-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.12.13/20260303/cpython-3.12.13+20260303-aarch64-unknown-linux-gnu-install_only.tar.gz",
        sha256: "15c00b489fb89c7e3dc433800cd8b932ab3f8825870c1919fbe747f493edf81d",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.12.13",
        build_tag: Some("20260303"),
        target: "x86_64-unknown-linux-gnu",
        archive_name: "cpython-3.12.13+20260303-x86_64-unknown-linux-gnu-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.12.13/20260303/cpython-3.12.13+20260303-x86_64-unknown-linux-gnu-install_only.tar.gz",
        sha256: "4e5ac5a04afc4fe164e92e3844d6dbda03b33baeb62e032b5c9a8198280221e2",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.12.13",
        build_tag: Some("20260303"),
        target: "aarch64-pc-windows-msvc",
        archive_name: "cpython-3.12.13+20260303-aarch64-pc-windows-msvc-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.12.13/20260303/cpython-3.12.13+20260303-aarch64-pc-windows-msvc-install_only.tar.gz",
        sha256: "f003d15139e0baa39563388b7e29c44a6c4dda3987dbd7cec2050eca32450d4f",
    },
    ManagedRuntimeArchiveSpec {
        kind: ManagedRuntimeKind::Python,
        version: "3.12.13",
        build_tag: Some("20260303"),
        target: "x86_64-pc-windows-msvc",
        archive_name: "cpython-3.12.13+20260303-x86_64-pc-windows-msvc-install_only.tar.gz",
        archive_kind: ManagedRuntimeArchiveKind::TarGz,
        mirror_url: "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/python/3.12.13/20260303/cpython-3.12.13+20260303-x86_64-pc-windows-msvc-install_only.tar.gz",
        sha256: "43990976c8de6b72a7525cb509eedaf869a8dd116167e708af08bca50cd8ef00",
    },
];

#[cfg(test)]
pub(crate) fn all_runtime_archive_specs() -> impl Iterator<Item = &'static ManagedRuntimeArchiveSpec>
{
    NODE_RUNTIME_ARCHIVES
        .iter()
        .chain(PYTHON_RUNTIME_ARCHIVES.iter())
}

pub(crate) fn resolve_node_runtime_archive(
    version: &str,
    target: &str,
    archive_kind: ManagedRuntimeArchiveKind,
) -> Result<&'static ManagedRuntimeArchiveSpec> {
    let spec = NODE_RUNTIME_ARCHIVES
        .iter()
        .find(|spec| {
            spec.version == version && spec.target == target && spec.archive_kind == archive_kind
        })
        .ok_or_else(|| {
            anyhow!(
                "managed Node runtime is not present in the ctx mirror lock: version={version}, target={target}"
            )
        })?;
    validate_runtime_archive_spec(spec)?;
    Ok(spec)
}

pub(crate) fn resolve_python_runtime_archive(
    version: &str,
    build_tag: &str,
    target: &str,
) -> Result<&'static ManagedRuntimeArchiveSpec> {
    let spec = PYTHON_RUNTIME_ARCHIVES
        .iter()
        .find(|spec| {
            spec.version == version && spec.build_tag == Some(build_tag) && spec.target == target
        })
        .ok_or_else(|| {
            anyhow!(
                "managed Python runtime is not present in the ctx mirror lock: version={version}, build={build_tag}, target={target}"
            )
        })?;
    validate_runtime_archive_spec(spec)?;
    Ok(spec)
}

pub(crate) fn validate_runtime_archive_spec(spec: &ManagedRuntimeArchiveSpec) -> Result<()> {
    validate_expected_sha256(spec.sha256).with_context(|| {
        format!(
            "invalid managed {} runtime sha256 for {}",
            spec.kind.as_str(),
            spec.archive_name
        )
    })?;
    let parsed = url::Url::parse(spec.mirror_url)
        .with_context(|| format!("invalid managed runtime mirror URL: {}", spec.mirror_url))?;
    if parsed.scheme() != "https" {
        anyhow::bail!(
            "managed runtime mirror URL must use https: {}",
            spec.mirror_url
        );
    }
    if parsed.host_str() != Some(MIRROR_HOST) {
        anyhow::bail!(
            "managed runtime mirror URL must use {MIRROR_HOST}: {}",
            spec.mirror_url
        );
    }
    if !parsed.path().starts_with(MIRROR_PATH_PREFIX) {
        anyhow::bail!(
            "managed runtime mirror URL must stay under {MIRROR_PATH_PREFIX}: {}",
            spec.mirror_url
        );
    }
    if !parsed.path().ends_with(spec.archive_name) {
        anyhow::bail!(
            "managed runtime mirror URL path must end with archive {}: {}",
            spec.archive_name,
            spec.mirror_url
        );
    }
    Ok(())
}

pub(crate) fn runtime_download_url_allowed(url: &url::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    url.host_str() == Some(MIRROR_HOST) && url.path().starts_with(MIRROR_PATH_PREFIX)
}

pub(crate) async fn runtime_ready_metadata_matches(
    root: &Path,
    spec: &ManagedRuntimeArchiveSpec,
) -> bool {
    let path = root.join(RUNTIME_READY_METADATA_FILENAME);
    let Ok(text) = tokio::fs::read_to_string(path).await else {
        return false;
    };
    let Ok(metadata) = serde_json::from_str::<RuntimeReadyMetadata>(&text) else {
        return false;
    };
    metadata.matches_spec(spec)
}

pub(crate) async fn write_runtime_ready_metadata(
    root: &Path,
    spec: &ManagedRuntimeArchiveSpec,
) -> Result<()> {
    let metadata = RuntimeReadyMetadata::from_spec(spec);
    let bytes = serde_json::to_vec_pretty(&metadata).context("serializing runtime metadata")?;
    let path = root.join(RUNTIME_READY_METADATA_FILENAME);
    let tmp_path = root.join(format!("{RUNTIME_READY_METADATA_FILENAME}.tmp"));
    tokio::fs::write(&tmp_path, bytes)
        .await
        .with_context(|| format!("writing runtime metadata: {}", tmp_path.display()))?;
    tokio::fs::rename(&tmp_path, &path).await.with_context(|| {
        format!(
            "committing runtime metadata {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

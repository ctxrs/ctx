use super::{
    metadata::{parse_release_metadata, project_managed_pair_release, ReleaseMetadata},
    TEST_SEMANTIC_LAYOUT,
};

const CORE_SHA256: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const COMPANION_SHA256: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn release_metadata(platform_key: &str, base_url: &str) -> String {
    format!(
        "\
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_CHANNEL=stable
CTX_RELEASE_VERSION=1.2.3
CTX_RELEASE_BASE_URL={base_url}
CTX_RELEASE_ARTIFACT_{platform_key}=ctx-{platform_key}
CTX_RELEASE_SHA256_{platform_key}={}
",
        "3".repeat(64)
    )
}

fn pair_fields(
    platform_key: &str,
    envelope: &str,
    core_object: &str,
    core_sha256: &str,
    companion_object: &str,
    companion_sha256: &str,
) -> [String; 5] {
    [
        format!("CTX_RELEASE_MANAGED_PAIR_ENVELOPE_{platform_key}={envelope}\n"),
        format!("CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_{platform_key}={core_object}\n"),
        format!("CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_{platform_key}={core_sha256}\n"),
        format!("CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_{platform_key}={companion_object}\n"),
        format!("CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_{platform_key}={companion_sha256}\n"),
    ]
}

fn valid_pair_fields(platform_key: &str, suffix: &str) -> [String; 5] {
    pair_fields(
        platform_key,
        &format!("managed-pair-{suffix}.json"),
        &format!("sha256/{CORE_SHA256}/ctx-{suffix}"),
        CORE_SHA256,
        &format!("sha256/{COMPANION_SHA256}/ctx-companion-{suffix}"),
        COMPANION_SHA256,
    )
}

fn parse(bytes: &str, platform: &str) -> anyhow::Result<ReleaseMetadata> {
    parse_release_metadata(
        bytes.as_bytes(),
        platform,
        "stable",
        false,
        &TEST_SEMANTIC_LAYOUT,
    )
}

#[test]
fn complete_managed_pair_fields_project_download_metadata() {
    let base_url = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.2.3";
    let mut bytes = release_metadata("linux_x64", base_url);
    bytes.push_str(&valid_pair_fields("linux_x64", "linux-x64").concat());

    let metadata = parse(&bytes, "linux-x64").unwrap();
    let release = project_managed_pair_release(&metadata.base_url, metadata.managed_pair.as_ref())
        .unwrap()
        .unwrap();

    assert_eq!(
        release.envelope_url,
        format!("{base_url}/managed-pair-linux-x64.json")
    );
    assert_eq!(
        release.core_object_url,
        format!("{base_url}/sha256/{CORE_SHA256}/ctx-linux-x64")
    );
    assert_eq!(release.core_sha256, CORE_SHA256);
    assert_eq!(
        release.companion_object_url,
        format!("{base_url}/sha256/{COMPANION_SHA256}/ctx-companion-linux-x64")
    );
    assert_eq!(release.companion_sha256, COMPANION_SHA256);
}

#[test]
fn managed_pair_fields_are_all_or_none() {
    let fields = valid_pair_fields("linux_x64", "linux-x64");
    for missing_index in 0..fields.len() {
        let mut bytes = release_metadata(
            "linux_x64",
            "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.2.3",
        );
        for (index, field) in fields.iter().enumerate() {
            if index != missing_index {
                bytes.push_str(field);
            }
        }

        let error = parse(&bytes, "linux-x64").unwrap_err();
        assert!(
            error.to_string().contains("managed-pair metadata")
                && error.to_string().contains("is partial"),
            "missing field {missing_index}: {error:#}"
        );
    }
}

#[test]
fn managed_pair_rejects_unsafe_envelope_names_and_object_keys() {
    let valid_core = format!("sha256/{CORE_SHA256}/ctx-linux-x64");
    let valid_companion = format!("sha256/{COMPANION_SHA256}/ctx-companion-linux-x64");
    let cases = [
        (
            "../managed-pair.json".to_owned(),
            valid_core.clone(),
            valid_companion.clone(),
        ),
        (
            "managed-pair.json".to_owned(),
            format!("sha512/{CORE_SHA256}/ctx-linux-x64"),
            valid_companion.clone(),
        ),
        (
            "managed-pair.json".to_owned(),
            format!("sha256/{CORE_SHA256}/nested/ctx-linux-x64"),
            valid_companion.clone(),
        ),
        (
            "managed-pair.json".to_owned(),
            valid_core,
            format!("sha256/{COMPANION_SHA256}/ctx companion"),
        ),
    ];

    for (envelope, core_object, companion_object) in cases {
        let mut bytes = release_metadata(
            "linux_x64",
            "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.2.3",
        );
        bytes.push_str(
            &pair_fields(
                "linux_x64",
                &envelope,
                &core_object,
                CORE_SHA256,
                &companion_object,
                COMPANION_SHA256,
            )
            .concat(),
        );

        let error = parse(&bytes, "linux-x64").unwrap_err();
        assert!(error.to_string().contains("unsafe"), "{error:#}");
    }
}

#[test]
fn managed_pair_rejects_object_key_digest_mismatch() {
    let mut bytes = release_metadata(
        "linux_x64",
        "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.2.3",
    );
    bytes.push_str(
        &pair_fields(
            "linux_x64",
            "managed-pair.json",
            &format!("sha256/{COMPANION_SHA256}/ctx-linux-x64"),
            CORE_SHA256,
            &format!("sha256/{COMPANION_SHA256}/ctx-companion-linux-x64"),
            COMPANION_SHA256,
        )
        .concat(),
    );

    let error = parse(&bytes, "linux-x64").unwrap_err();
    assert!(
        error.to_string().contains("digest does not match"),
        "{error:#}"
    );
}

#[test]
fn managed_pair_metadata_selects_the_requested_platform() {
    let base_url = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.2.3";
    let mut bytes = release_metadata("macos_x64", base_url);
    bytes.push_str(&valid_pair_fields("linux_x64", "linux-x64").concat());
    bytes.push_str(&valid_pair_fields("macos_x64", "macos-x64").concat());

    let metadata = parse(&bytes, "macos-x64").unwrap();
    let release = project_managed_pair_release(&metadata.base_url, metadata.managed_pair.as_ref())
        .unwrap()
        .unwrap();

    assert!(release
        .envelope_url
        .ends_with("/managed-pair-macos-x64.json"));
    assert!(release.core_object_url.ends_with("/ctx-macos-x64"));
    assert!(release
        .companion_object_url
        .ends_with("/ctx-companion-macos-x64"));
}

#[test]
fn absent_managed_pair_fields_preserve_core_only_metadata() {
    let metadata = parse(
        &release_metadata(
            "linux_x64",
            "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.2.3",
        ),
        "linux-x64",
    )
    .unwrap();

    assert!(metadata.managed_pair.is_none());
    assert!(
        project_managed_pair_release(&metadata.base_url, metadata.managed_pair.as_ref())
            .unwrap()
            .is_none()
    );
}

#[test]
fn managed_pair_projection_uses_existing_release_base_authority() {
    let mut bytes = release_metadata("linux_x64", "https://attacker.invalid/artifacts");
    bytes.push_str(&valid_pair_fields("linux_x64", "linux-x64").concat());
    let metadata = parse(&bytes, "linux-x64").unwrap();

    let error = project_managed_pair_release(&metadata.base_url, metadata.managed_pair.as_ref())
        .unwrap_err();
    assert!(error.to_string().contains("metadata base URL"), "{error:#}");
}

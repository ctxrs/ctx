#[cfg(windows)]
use super::super::hosted_pair_install::validate_hosted_marker_path;
use super::super::hosted_pair_install::HostedInstallMarker;

#[test]
fn hosted_marker_channel_is_distribution_only() {
    let staging = HostedInstallMarker {
        schema_version: 1,
        manager: "ctx-hosted-installer".to_owned(),
        install_path: "/tmp/ctx".to_owned(),
        platform: "linux-x64".to_owned(),
        channel: "stable".to_owned(),
        sha256: "1".repeat(64),
        staging_dogfood: true,
    };
    assert_eq!(
        staging.release_channel().unwrap(),
        ctx_companion_bridge::ReleaseChannel::Staging
    );
}

#[cfg(windows)]
#[test]
fn windows_hosted_pair_marker_accepts_only_ordinary_verbatim_equivalence() {
    let marker = |install_path: &str| HostedInstallMarker {
        schema_version: 1,
        manager: "ctx-hosted-installer".to_owned(),
        install_path: install_path.to_owned(),
        platform: "windows-x64".to_owned(),
        channel: "stable".to_owned(),
        sha256: "1".repeat(64),
        staging_dogfood: false,
    };
    let certified = std::path::Path::new(r"\\?\C:\Users\ctx\bin\ctx.exe");

    validate_hosted_marker_path(&marker(r"C:\Users\ctx\bin\ctx.exe"), certified).unwrap();
    validate_hosted_marker_path(&marker(r"\\?\C:\Users\ctx\bin\ctx.exe"), certified).unwrap();

    for rejected in [
        r"C:\Users\CTX\bin\ctx.exe",
        r"C:\Users\ctx\other\..\bin\ctx.exe",
        r"\\server\share\ctx.exe",
        r"\\.\C:\Users\ctx\bin\ctx.exe",
    ] {
        assert!(
            validate_hosted_marker_path(&marker(rejected), certified).is_err(),
            "accepted unsafe or aliased hosted-pair marker path {rejected}"
        );
    }
}

use super::*;
use ctx_core::ids::{ConnectionProfileId, MobileDeviceId, WorkspaceId};

const TEST_MOBILE_API_TOKEN: &str = "ctxm_test_mobile_api_token";
const TEST_MOBILE_DEFAULT_SCOPES: &[&str] =
    &["device_registration", "workspace_read", "workspace_stream"];

pub(super) async fn insert_mobile_profile(daemon: &TestDaemon) -> ConnectionProfileId {
    insert_mobile_profile_with_scopes(daemon, TEST_MOBILE_DEFAULT_SCOPES).await
}

pub(super) async fn insert_mobile_profile_with_scopes(
    daemon: &TestDaemon,
    scopes: &[&str],
) -> ConnectionProfileId {
    daemon
        .mobile_access_for_test()
        .seed_mobile_api_profile_for_test(TEST_MOBILE_API_TOKEN, scopes)
        .await
        .unwrap()
        .id
}

pub(super) async fn build_mobile_access_app(
    enabled: bool,
) -> (
    axum::Router,
    DataRootTestDaemonFixture,
    WorkspaceId,
    String,
    ctx_transport_runtime::mobile_e2ee::E2eeKey,
    tempfile::TempDir,
) {
    build_mobile_access_app_with_scopes(enabled, TEST_MOBILE_DEFAULT_SCOPES).await
}

pub(super) async fn build_mobile_access_app_with_scopes(
    enabled: bool,
    scopes: &[&str],
) -> (
    axum::Router,
    DataRootTestDaemonFixture,
    WorkspaceId,
    String,
    ctx_transport_runtime::mobile_e2ee::E2eeKey,
    tempfile::TempDir,
) {
    let git_repo = setup_git_repo().await;
    let data_dir = tempfile::tempdir().unwrap();
    let daemon = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = daemon.router();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;
    let profile_id = insert_mobile_profile_with_scopes(daemon.daemon(), scopes).await;
    let device_id = "22222222-2222-2222-2222-222222222222".to_string();
    let (daemon_public_key, daemon_private_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    let (device_public_key, device_secret_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();

    seed_mobile_access_config(
        daemon.daemon(),
        profile_id,
        &device_id,
        daemon_public_key.clone(),
        daemon_private_key,
        device_public_key,
        enabled,
    )
    .await;

    let key = ctx_transport_runtime::mobile_e2ee::derive_client_key(
        &device_id,
        &device_secret_key,
        &daemon_public_key,
    )
    .unwrap();

    (app, daemon, workspace.id, device_id, key, data_dir)
}

pub(super) async fn build_mobile_secure_proxy_app(
    enabled: bool,
) -> (
    axum::Router,
    DataRootTestDaemonFixture,
    String,
    ctx_transport_runtime::mobile_e2ee::E2eeKey,
    tempfile::TempDir,
) {
    build_mobile_secure_proxy_app_with_scopes(enabled, TEST_MOBILE_DEFAULT_SCOPES).await
}

pub(super) async fn build_mobile_secure_proxy_app_with_scopes(
    enabled: bool,
    scopes: &[&str],
) -> (
    axum::Router,
    DataRootTestDaemonFixture,
    String,
    ctx_transport_runtime::mobile_e2ee::E2eeKey,
    tempfile::TempDir,
) {
    let data_dir = tempfile::tempdir().unwrap();
    let daemon =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let profile_id = insert_mobile_profile_with_scopes(daemon.daemon(), scopes).await;

    let device_id = "44444444-4444-4444-4444-444444444444".to_string();
    let (daemon_public_key, daemon_private_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    let (device_public_key, device_secret_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    seed_mobile_access_config(
        daemon.daemon(),
        profile_id,
        &device_id,
        daemon_public_key.clone(),
        daemon_private_key,
        device_public_key,
        enabled,
    )
    .await;

    let key = ctx_transport_runtime::mobile_e2ee::derive_client_key(
        &device_id,
        &device_secret_key,
        &daemon_public_key,
    )
    .unwrap();
    let app = daemon.router();
    (app, daemon, device_id, key, data_dir)
}

async fn seed_mobile_access_config(
    daemon: &TestDaemon,
    profile_id: ConnectionProfileId,
    device_id: &str,
    daemon_public_key: String,
    daemon_private_key: String,
    device_public_key: String,
    enabled: bool,
) {
    daemon
        .mobile_access_for_test()
        .seed_default_mobile_access_config_for_test(
            profile_id,
            enabled,
            daemon_public_key,
            daemon_private_key,
        )
        .await
        .unwrap();
    daemon
        .mobile_access_for_test()
        .seed_mobile_device_for_test(
            MobileDeviceId(uuid::Uuid::parse_str(device_id).unwrap()),
            profile_id,
            device_public_key,
            "phone",
        )
        .await
        .unwrap();
}

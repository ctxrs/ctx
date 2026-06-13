mod common;

use ctx_managed_installs::title_generation_local;
use ctx_session_title_service::title_generation;
use ctx_settings_model::{
    TitleGenerationLocalSettings, TitleGenerationMode, TitleGenerationSettings,
};

#[tokio::test]
async fn generate_title_local_uses_mock_llama_server() {
    let data_dir = tempfile::tempdir().unwrap();

    let model_path = title_generation_local::model_path(data_dir.path());
    if let Some(parent) = model_path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(&model_path, b"mock").await.unwrap();

    let runtime_dir = title_generation_local::runtime_dir(data_dir.path());
    tokio::fs::create_dir_all(&runtime_dir).await.unwrap();

    let mock_path = common::resolve_cargo_bin_exe(env!("CARGO_BIN_EXE_llama_server_mock"));
    let runtime_path = runtime_dir.join(title_generation_local::runtime_binary_name());
    tokio::fs::copy(&mock_path, &runtime_path).await.unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = std::fs::metadata(&runtime_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&runtime_path, perms).unwrap();
    }

    let cfg = TitleGenerationSettings {
        mode: TitleGenerationMode::Local,
        local: TitleGenerationLocalSettings {
            model_id: title_generation_local::LOCAL_MODEL_ID.to_string(),
            use_json: true,
        },
        ..Default::default()
    };

    let title = title_generation::generate_title(&cfg, "mock prompt", data_dir.path())
        .await
        .unwrap();
    assert_eq!(title, "Mock Title");
}

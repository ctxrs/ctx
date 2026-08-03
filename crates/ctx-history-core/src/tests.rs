use std::{env, path::PathBuf};

use crate::{
    config_path, default_data_root, device_path, history_dir, logs_dir, managed_data_root,
    CaptureProvider, Confidence, Fidelity,
};

#[test]
fn obsolete_content_reference_surface_is_absent() {
    let crate_root = include_str!("lib.rs");
    for removed in [concat!("Content", "Ref"), concat!("mod content", "_ref;")] {
        assert!(!crate_root.contains(removed), "found {removed}");
    }
}

#[test]
fn enum_string_roundtrips_and_defaults() {
    assert_eq!(Fidelity::default(), Fidelity::Partial);
    assert_eq!(Confidence::default(), Confidence::Unknown);
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"copilot_cli\"").unwrap(),
        CaptureProvider::CopilotCli
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"factory_ai_droid\"").unwrap(),
        CaptureProvider::FactoryAiDroid
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"kilo\"").unwrap(),
        CaptureProvider::Kilo
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"kiro_cli\"").unwrap(),
        CaptureProvider::KiroCli
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"qwen_code\"").unwrap(),
        CaptureProvider::QwenCode
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"kimi_code_cli\"").unwrap(),
        CaptureProvider::KimiCodeCli
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"forgecode\"").unwrap(),
        CaptureProvider::ForgeCode
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"mistral_vibe\"").unwrap(),
        CaptureProvider::MistralVibe
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"mux\"").unwrap(),
        CaptureProvider::Mux
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"rovodev\"").unwrap(),
        CaptureProvider::RovoDev
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"lingma\"").unwrap(),
        CaptureProvider::Lingma
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"mimocode\"").unwrap(),
        CaptureProvider::MiMoCode
    );
}

#[test]
fn retained_local_layout_paths_are_flat_under_data_root() {
    let root = PathBuf::from("/tmp/ctx-root");
    assert_eq!(history_dir(root.clone()), PathBuf::from("/tmp/ctx-root"));
    assert_eq!(
        config_path(root.clone()),
        PathBuf::from("/tmp/ctx-root/config.toml")
    );
    assert_eq!(logs_dir(root.clone()), PathBuf::from("/tmp/ctx-root/logs"));
    assert_eq!(
        device_path(root),
        PathBuf::from("/tmp/ctx-root/device.json")
    );
}

#[test]
fn ctx_data_root_env_is_the_ctx_root_itself() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    let _guard = ENV_LOCK.lock().unwrap();
    let previous = env::var_os("CTX_DATA_ROOT");
    env::remove_var("CTX_DATA_ROOT");

    let default_root = default_data_root().unwrap();
    assert!(default_root.ends_with(".ctx"));
    let managed_root = managed_data_root().unwrap();
    assert_eq!(managed_root, default_root);

    env::set_var("CTX_DATA_ROOT", "/tmp/custom-ctx-root");

    assert_eq!(
        default_data_root().unwrap(),
        PathBuf::from("/tmp/custom-ctx-root")
    );
    assert_eq!(managed_data_root().unwrap(), managed_root);

    if let Some(previous) = previous {
        env::set_var("CTX_DATA_ROOT", previous);
    } else {
        env::remove_var("CTX_DATA_ROOT");
    }
}

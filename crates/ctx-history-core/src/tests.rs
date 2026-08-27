use crate::{CaptureProvider, Fidelity};

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
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"copilot_cli\"").unwrap(),
        CaptureProvider::CopilotCli
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"grok_build\"").unwrap(),
        CaptureProvider::GrokBuild
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"deepseek_harness\"").unwrap(),
        CaptureProvider::DeepSeekHarness
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
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"fx\"").unwrap(),
        CaptureProvider::Fx
    );
}

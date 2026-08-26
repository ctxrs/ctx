use crate::{
    native_path::{
        antigravity_source_backed_adapter, copilot_source_backed_adapter,
        factory_droid_source_backed_adapter, grok_build_source_backed_adapter,
        qoder_source_backed_adapter, qwen_code_source_backed_adapter,
        tabnine_source_backed_adapter,
    },
    test_support::NativeJsonlTestRuntime,
};

#[test]
fn direct_jsonl_source_and_session_identities_are_root_scoped() {
    let adapters = [
        antigravity_source_backed_adapter::<NativeJsonlTestRuntime>(),
        copilot_source_backed_adapter::<NativeJsonlTestRuntime>(),
        factory_droid_source_backed_adapter::<NativeJsonlTestRuntime>(),
        grok_build_source_backed_adapter::<NativeJsonlTestRuntime>(),
        qoder_source_backed_adapter::<NativeJsonlTestRuntime>(),
        qwen_code_source_backed_adapter::<NativeJsonlTestRuntime>(),
        tabnine_source_backed_adapter::<NativeJsonlTestRuntime>(),
    ];

    for adapter in adapters {
        let released = adapter.source_key("shared-native-session").unwrap();
        let compatibility = adapter
            .with_source_root_lineage(None)
            .source_key("shared-native-session")
            .unwrap();
        let first = adapter
            .with_source_root_lineage(Some([1; 32]))
            .session_identity("shared-native-session")
            .unwrap();
        let second = adapter
            .with_source_root_lineage(Some([2; 32]))
            .session_identity("shared-native-session")
            .unwrap();

        assert!(released.exact_descriptor_eq(&compatibility));
        assert_ne!(released.identity(), first.0.identity());
        assert_ne!(first.0.identity(), second.0.identity());
        assert_ne!(first.1, second.1);
    }
}

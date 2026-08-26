use std::collections::BTreeSet;

use super::*;

#[test]
fn provider_vocabulary_keeps_all_41_recognized_native_providers_importable() {
    let recognized = native_provider_cli_specs()
        .iter()
        .map(|spec| spec.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(recognized.len(), 41, "recognized provider count changed");
    let registered = ctx_history_capture::provider_source_specs()
        .iter()
        .map(|spec| spec.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(recognized, registered, "provider vocabulary drifted");

    let importable = recognized
        .iter()
        .filter(|provider| parse_native_provider_name(provider).is_some_and(provider_is_importable))
        .collect::<BTreeSet<_>>();
    assert_eq!(importable.len(), 41, "importable provider count changed");
    assert!(provider_is_importable(CaptureProvider::Hermes));
    assert!(cli_supported_provider(CaptureProvider::Hermes));
}

#[test]
fn vocabulary_accepts_primary_storage_and_compatibility_names() {
    for spec in provider_cli_specs() {
        assert_eq!(
            parse_capture_provider_name(spec.cli_name),
            Some(spec.provider),
            "{} primary CLI name drifted",
            spec.cli_name
        );
        assert_eq!(
            parse_provider_name(spec.cli_name),
            Some(HistoryProvider::from(spec.provider)),
            "{} primary CLI transport name drifted",
            spec.cli_name
        );
        assert_eq!(
            parse_capture_provider_name(spec.provider.as_str()),
            Some(spec.provider),
            "{} storage name drifted",
            spec.provider.as_str()
        );
        assert_eq!(
            parse_provider_name(spec.provider.as_str()),
            Some(HistoryProvider::from(spec.provider)),
            "{} storage transport name drifted",
            spec.provider.as_str()
        );
        for alias in spec.aliases {
            assert_eq!(
                parse_capture_provider_name(alias),
                Some(spec.provider),
                "{alias} compatibility alias drifted"
            );
            assert_eq!(
                parse_provider_name(alias),
                Some(HistoryProvider::from(spec.provider)),
                "{alias} compatibility transport alias drifted"
            );
        }
    }
    assert_eq!(
        parse_capture_provider_name("custom"),
        Some(CaptureProvider::Custom),
        "Custom remains part of the complete public provider vocabulary"
    );
    assert_eq!(
        parse_native_provider_name("custom"),
        None,
        "Custom remains a public-only provider"
    );
}

#[test]
fn mcp_names_include_primary_and_storage_names_without_duplicates() {
    let names = mcp_provider_names();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(names.contains(&"kiro-cli"));
    assert!(names.contains(&"kiro_cli"));
    assert!(names.contains(&"grok-build"));
    assert!(names.contains(&"grok_build"));
    assert!(names.contains(&"deepseek-harness"));
    assert!(names.contains(&"deepseek_harness"));
    assert!(names.contains(&"custom"));
}

#[test]
fn grok_build_uses_canonical_cli_name_and_documented_alias() {
    for name in ["grok-build", "grok", "grok_build"] {
        assert_eq!(
            parse_native_provider_name(name),
            Some(CaptureProvider::GrokBuild),
            "{name} did not resolve to Grok Build"
        );
    }
    assert_eq!(provider_cli_name(CaptureProvider::GrokBuild), "grok-build");
    assert_eq!(parse_native_provider_name("grokbuild"), None);
}

#[test]
fn deepseek_harness_uses_canonical_cli_name_and_narrow_aliases() {
    for name in ["deepseek-harness", "dsh", "deepseek_harness"] {
        assert_eq!(
            parse_native_provider_name(name),
            Some(CaptureProvider::DeepSeekHarness),
            "{name} did not resolve to DeepSeek Harness"
        );
    }
    assert_eq!(
        provider_cli_name(CaptureProvider::DeepSeekHarness),
        "deepseek-harness"
    );
    assert_eq!(parse_native_provider_name("deepseek"), None);
}

#[test]
fn unknown_provider_error_stays_compact() {
    assert_eq!(
        parse_provider("not-a-provider").unwrap_err(),
        compact_provider_error("not-a-provider")
    );
}

#[test]
fn invalid_provider_names_are_rejected_by_every_public_parser() {
    for name in [
        "",
        "grokbuild",
        "Grok-Build",
        "grok build",
        "not-a-provider",
    ] {
        assert_eq!(parse_capture_provider_name(name), None, "{name:?}");
        assert_eq!(parse_provider_name(name), None, "{name:?}");
        assert_eq!(parse_native_provider_name(name), None, "{name:?}");
        assert!(parse_provider(name).is_err(), "{name:?}");
        assert!(parse_native_provider(name).is_err(), "{name:?}");
    }
}

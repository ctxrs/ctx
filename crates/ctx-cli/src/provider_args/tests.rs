use std::collections::BTreeSet;

use super::*;

fn importable_provider_names() -> BTreeSet<&'static str> {
    ctx_history_capture::provider_source_specs()
        .iter()
        .filter(|spec| spec.import_support.is_importable())
        .map(|spec| spec.provider.as_str())
        .collect()
}

#[test]
fn cli_provider_enums_are_the_exact_41_semantic_providers_plus_custom() {
    let importable = importable_provider_names();
    assert_eq!(importable.len(), 41, "semantic provider count changed");

    let native_variants = NativeProviderArg::value_variants();
    let native_providers = native_variants
        .iter()
        .map(|provider| provider.capture_provider().as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        native_providers.len(),
        native_variants.len(),
        "native CLI enum maps multiple values to one CaptureProvider"
    );
    assert_eq!(native_providers, importable);

    let public_variants = ProviderArg::value_variants();
    let public_providers = public_variants
        .iter()
        .map(|provider| provider.capture_provider().as_str())
        .collect::<BTreeSet<_>>();
    let mut expected_public = importable.clone();
    assert!(expected_public.insert(CaptureProvider::Custom.as_str()));
    assert_eq!(public_variants.len(), 42, "public provider count changed");
    assert_eq!(
        public_providers.len(),
        public_variants.len(),
        "public CLI enum maps multiple values to one CaptureProvider"
    );
    assert_eq!(public_providers, expected_public);
}

#[test]
fn every_cli_provider_name_round_trips_to_its_registered_capture_provider() {
    for spec in ctx_history_capture::provider_source_specs()
        .iter()
        .filter(|spec| spec.import_support.is_importable())
    {
        let storage_name = spec.provider.as_str();
        let native = parse_native_provider_arg(storage_name).unwrap_or_else(|error| {
            panic!("{storage_name} is absent from the native import CLI: {error}")
        });
        assert_eq!(
            native.capture_provider(),
            spec.provider,
            "{storage_name} native CLI mapping drifted"
        );

        let public = parse_provider_arg(storage_name).unwrap_or_else(|error| {
            panic!("{storage_name} is absent from the public CLI: {error}")
        });
        assert_eq!(
            public.capture_provider(),
            spec.provider,
            "{storage_name} public CLI mapping drifted"
        );
    }

    for native in NativeProviderArg::value_variants() {
        let cli_name = native
            .to_possible_value()
            .expect("native CLI provider must have a clap value")
            .get_name()
            .to_owned();
        let public = parse_provider_arg(&cli_name)
            .unwrap_or_else(|error| panic!("{cli_name} is absent from the public CLI: {error}"));
        assert_eq!(
            public.capture_provider(),
            native.capture_provider(),
            "{cli_name} maps differently across CLI provider enums"
        );
    }

    assert_eq!(
        parse_provider_arg(CaptureProvider::Custom.as_str())
            .expect("Custom must remain a public provider")
            .capture_provider(),
        CaptureProvider::Custom
    );
}

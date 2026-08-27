use super::*;

#[test]
fn semantic_configuration_uses_sparse_defaults_when_absent() {
    let values = parse_toml_subset("[upgrade]\nauto = \"off\"\n").unwrap();
    let mut config = AppConfig::default();

    config.apply_values(&values).unwrap();

    assert_eq!(config.semantic.enabled, None);
    assert!(!config.semantic_search_enabled());
    assert_eq!(config.semantic_search_source(), "default");
    assert_eq!(
        config.semantic_indexing_intensity(),
        SemanticIndexingIntensity::Quiet
    );
    assert_eq!(config.semantic_indexing_intensity_source(), "default");
}

#[test]
fn canonical_semantic_enabled_and_released_legacy_key_are_readable() {
    for (text, expected) in [
        ("[semantic]\nenabled = true\n", true),
        ("[semantic]\nenabled = false\n", false),
        ("[search]\nsemantic = true\n", true),
        ("[search]\nsemantic = false\n", false),
    ] {
        let values = parse_toml_subset(text).unwrap();
        let mut config = AppConfig::default();

        config.apply_values(&values).unwrap();

        assert_eq!(config.semantic.enabled, Some(expected), "{text}");
        assert_eq!(config.semantic_search_enabled(), expected, "{text}");
        assert_eq!(config.semantic_search_source(), "config", "{text}");
    }
}

#[test]
fn canonical_semantic_enabled_wins_legacy_conflicts_regardless_of_file_order() {
    for (text, expected) in [
        (
            "[search]\nsemantic = true\n\n[semantic]\nenabled = false\n",
            false,
        ),
        (
            "[semantic]\nenabled = false\n\n[search]\nsemantic = true\n",
            false,
        ),
        (
            "[search]\nsemantic = false\n\n[semantic]\nenabled = true\n",
            true,
        ),
        (
            "[semantic]\nenabled = true\n\n[search]\nsemantic = false\n",
            true,
        ),
    ] {
        let values = parse_toml_subset(text).unwrap();
        let mut config = AppConfig::default();

        config.apply_values(&values).unwrap();

        assert_eq!(config.semantic.enabled, Some(expected), "{text}");
        assert_eq!(config.semantic_search_enabled(), expected, "{text}");
        assert_eq!(config.semantic_search_source(), "config", "{text}");
    }
}

#[test]
fn semantic_indexing_intensity_parses_exact_closed_values() {
    assert_eq!(
        SemanticIndexingIntensity::default(),
        SemanticIndexingIntensity::Quiet
    );
    for (value, expected) in [
        ("quiet", SemanticIndexingIntensity::Quiet),
        ("full", SemanticIndexingIntensity::Full),
    ] {
        let values =
            parse_toml_subset(&format!("[semantic]\nindexing_intensity = \"{value}\"\n")).unwrap();
        let mut config = AppConfig::default();

        config.apply_values(&values).unwrap();

        assert_eq!(config.semantic_indexing_intensity(), expected);
        assert_eq!(config.semantic_indexing_intensity().as_str(), value);
        assert_eq!(config.semantic_indexing_intensity_source(), "config");
    }

    for value in ["Quiet", "FULL", "unlimited", " quiet"] {
        let error = load_config_error(format!("[semantic]\nindexing_intensity = \"{value}\"\n"));
        assert!(error.contains("semantic.indexing_intensity"), "{error}");
        assert!(error.contains("\"quiet\" or \"full\""), "{error}");
    }
}

#[test]
fn semantic_enablement_mutation_is_canonical_preserving_and_idempotent() {
    let _env_guard = EnvGuard::new(&["CTX_SEARCH_SEMANTIC"]);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    let original = "# retained comment\n[analytics]\nenabled = false\n\n[search]\nsemantic = false\n\n[semantic]\nindexing_intensity = \"full\"\n";
    fs::write(&path, original).unwrap();

    set_semantic_search_enabled(temp.path(), true).unwrap();
    let enabled = AppConfig::load(temp.path()).unwrap();
    assert!(enabled.semantic_search_enabled());
    assert_eq!(enabled.semantic_search_source(), "config");
    assert_eq!(
        enabled.semantic_indexing_intensity(),
        SemanticIndexingIntensity::Full
    );
    let once = fs::read_to_string(&path).unwrap();
    assert!(once.starts_with("# retained comment\n[analytics]\nenabled = false\n"));
    assert!(!once.contains("[search]"), "{once}");
    assert!(!once.contains("search.semantic"), "{once}");
    assert!(once.contains("[semantic]"), "{once}");
    assert!(once.contains("enabled = true"), "{once}");
    assert!(once.contains("indexing_intensity = \"full\""), "{once}");

    set_semantic_search_enabled(temp.path(), true).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), once);

    set_semantic_search_enabled(temp.path(), false).unwrap();
    let disabled = AppConfig::load(temp.path()).unwrap();
    assert!(!disabled.semantic_search_enabled());
    assert_eq!(disabled.semantic_search_source(), "default");
    assert_eq!(
        disabled.semantic_indexing_intensity(),
        SemanticIndexingIntensity::Full
    );
    let disabled_once = fs::read_to_string(&path).unwrap();
    assert!(
        !parse_toml_subset(&disabled_once)
            .unwrap()
            .contains_key("semantic.enabled"),
        "{disabled_once}"
    );
    assert!(
        disabled_once.contains("indexing_intensity = \"full\""),
        "{disabled_once}"
    );
    set_semantic_search_enabled(temp.path(), false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), disabled_once);
}

#[test]
fn semantic_disable_sparse_canonicalization_removes_quiet_and_empty_tables() {
    let _env_guard = EnvGuard::new(&["CTX_SEARCH_SEMANTIC"]);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    fs::write(
        &path,
        "[search]\nsemantic = true\n\n[semantic]\nenabled = true\nindexing_intensity = \"quiet\"\n",
    )
    .unwrap();

    set_semantic_search_enabled(temp.path(), false).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "");
    let config = AppConfig::load(temp.path()).unwrap();
    assert!(!config.semantic_search_enabled());
    assert_eq!(
        config.semantic_indexing_intensity(),
        SemanticIndexingIntensity::Quiet
    );
    assert_eq!(config.semantic_indexing_intensity_source(), "default");
    set_semantic_search_enabled(temp.path(), false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
}

#[test]
fn semantic_sparse_mutation_preserves_comments_and_decorated_tables() {
    let _env_guard = EnvGuard::new(&["CTX_SEARCH_SEMANTIC"]);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    fs::write(
        &path,
        "[search] # legacy header\nsemantic = true # legacy value\n\n[semantic] # canonical header\nenabled = true # enabled value\nindexing_intensity = \"quiet\" # quiet value\n",
    )
    .unwrap();

    set_semantic_search_enabled(temp.path(), false).unwrap();

    let updated = fs::read_to_string(&path).unwrap();
    assert!(updated.contains("[search] # legacy header"), "{updated}");
    assert!(updated.contains("# legacy value"), "{updated}");
    assert!(
        updated.contains("[semantic] # canonical header"),
        "{updated}"
    );
    assert!(updated.contains("# enabled value"), "{updated}");
    assert!(updated.contains("# quiet value"), "{updated}");
    assert!(!updated.contains("semantic = true"), "{updated}");
    assert!(!updated.contains("enabled = true"), "{updated}");
    assert!(!updated.contains("indexing_intensity ="), "{updated}");
    assert!(parse_toml_subset(&updated).unwrap().is_empty());
}

#[test]
fn semantic_environment_override_is_final_over_canonical_and_legacy_config() {
    let env_guard = EnvGuard::new(&["CTX_SEARCH_SEMANTIC"]);
    let temp = tempfile::tempdir().unwrap();

    fs::write(
        temp.path().join(CONFIG_FILE),
        "[search]\nsemantic = true\n\n[semantic]\nenabled = false\n",
    )
    .unwrap();
    env_guard.set("CTX_SEARCH_SEMANTIC", "true");
    let config = AppConfig::load(temp.path()).unwrap();
    assert_eq!(config.semantic.enabled, Some(true));
    assert_eq!(config.semantic_search_source(), "environment");

    fs::write(
        temp.path().join(CONFIG_FILE),
        "[semantic]\nenabled = true\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    env_guard.set("CTX_SEARCH_SEMANTIC", "false");
    let config = AppConfig::load(temp.path()).unwrap();
    assert_eq!(config.semantic.enabled, Some(false));
    assert_eq!(config.semantic_search_source(), "environment");
}

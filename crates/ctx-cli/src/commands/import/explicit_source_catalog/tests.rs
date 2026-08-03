#[cfg(test)]
mod tests {
    use super::*;

    fn custom_source(path: PathBuf) -> ProviderSource {
        custom_provider_source(path, true).unwrap()
    }

    #[test]
    fn exact_source_registration_is_an_inline_request_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"\n").unwrap();

        let request = upsert_explicit_source(&data_root, &custom_source(path.clone())).unwrap();

        assert_eq!(request.path, path);
        assert_eq!(request.authority.route_lineages().len(), 1);
        assert_eq!(
            ExplicitSourceCatalogAuthority::from_json(&request.authority.to_json()).unwrap(),
            request.authority
        );
        assert!(!data_root.join("catalogs/explicit-sources").exists());
    }

    #[test]
    fn request_lineage_is_stable_per_exact_path_and_distinct_across_paths() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let first = temp.path().join("first.jsonl");
        let second = temp.path().join("second.jsonl");
        fs::write(&first, b"\n").unwrap();
        fs::write(&second, b"\n").unwrap();

        let first_request = upsert_explicit_source(&data_root, &custom_source(first.clone())).unwrap();
        let repeated = upsert_explicit_source(&data_root, &custom_source(first)).unwrap();
        let second_request = upsert_explicit_source(&data_root, &custom_source(second)).unwrap();

        assert_eq!(first_request.catalog_lineage, repeated.catalog_lineage);
        assert_ne!(first_request.catalog_lineage, second_request.catalog_lineage);
    }

    #[test]
    fn request_overlay_cannot_encode_deletion_authority() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"\n").unwrap();
        let request = upsert_explicit_source(&temp.path().join("data"), &custom_source(path)).unwrap();
        let mut entries = request.authority.entries.clone();
        entries[0].enabled = false;
        let error = sort_and_validate_entries(&mut entries).unwrap_err();
        assert!(format!("{error:#}").contains("cannot authorize deletion"));
    }

}

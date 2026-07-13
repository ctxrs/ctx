use std::collections::HashSet;

use super::super::{provider_source_specs, PROVIDER_IMPORT_REVISIONS};

#[test]
fn import_revision_registry_covers_default_provider_formats_without_duplicates() {
    let mut keys = HashSet::new();
    for entry in PROVIDER_IMPORT_REVISIONS {
        assert!(entry.revision > 0);
        assert!(
            keys.insert((entry.provider, entry.source_format)),
            "duplicate import revision for {}/{}",
            entry.provider.as_str(),
            entry.source_format
        );
    }

    for spec in provider_source_specs() {
        for location in spec.default_locations {
            assert!(
                PROVIDER_IMPORT_REVISIONS.iter().any(|entry| {
                    entry.provider == spec.provider && entry.source_format == location.source_format
                }),
                "missing import revision for {}/{}",
                spec.provider.as_str(),
                location.source_format
            );
        }
    }
}

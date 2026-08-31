use std::path::Path;

use anyhow::Result;
use ctx_history_capture::{source_backed_source_failure_identity, ProviderSource};
use ctx_history_core::CaptureProvider;
use ctx_history_ingest_application::{
    HistorySourcePluginSource, ImportIndexFacts, ImportPathMissingDuringRefresh, IngestPublication,
    RefreshSelection,
};
use ctx_history_platform::platform_security::establish_private_data_root;
use ctx_history_refresh::ExplicitSourceCatalogUpsert;

use crate::{
    history_source_plugins::prepare_source_backed_history_source, progress::ProgressReporter,
};

use super::{
    core_refresh::{is_terminal_missing_import_path, wait_for_import_core_refresh},
    explicit_source_catalog::{
        explicit_source_for_admission, relocate_explicit_source, relocation_authority_for_import,
        upsert_explicit_source,
    },
};

/// Final-host implementation of the static import application port. This is
/// deliberately limited to concrete filesystem/catalog/daemon side effects.
pub(super) struct CliImportHost {
    semantic_completion: crate::semantic::ImportSemanticCompletion,
}

impl CliImportHost {
    pub(super) fn new(semantic_completion: crate::semantic::ImportSemanticCompletion) -> Self {
        Self {
            semantic_completion,
        }
    }
}

impl ctx_history_cli::ImportApplicationPort for CliImportHost {
    fn protect_data_root(&mut self, data_root: &Path) -> Result<()> {
        establish_private_data_root(data_root).map_err(anyhow::Error::new)
    }

    fn explicit_source(
        &self,
        data_root: &Path,
        path: &Path,
        provider: Option<CaptureProvider>,
        custom_jsonl: bool,
    ) -> Result<ProviderSource> {
        explicit_source_for_admission(data_root, path, provider, custom_jsonl)
    }

    fn prepare_plugin(
        &mut self,
        source: &HistorySourcePluginSource,
        reset_cursor: bool,
    ) -> Result<ProviderSource> {
        Ok(
            prepare_source_backed_history_source(source.clone(), reset_cursor)?
                .provider_source()
                .clone(),
        )
    }

    fn admit_exact(
        &mut self,
        data_root: &Path,
        source: &ProviderSource,
        relocate_from: Option<&Path>,
    ) -> Result<ExplicitSourceCatalogUpsert> {
        if let Some(old_path) = relocate_from {
            let relocation = relocation_authority_for_import(data_root, old_path)?;
            relocate_explicit_source(data_root, source, relocation)
        } else {
            upsert_explicit_source(data_root, source)
        }
    }

    fn source_failure_identity(&self, source: &ProviderSource) -> Result<String> {
        source_backed_source_failure_identity(source).map_err(anyhow::Error::from)
    }

    fn refresh(
        &mut self,
        data_root: &Path,
        selection: RefreshSelection,
        no_daemon: bool,
        progress: &mut ProgressReporter<'_>,
    ) -> Result<IngestPublication> {
        let exact_route_lineages = selection
            .explicit_source_authority()
            .map(|authority| authority.route_lineages());
        let refresh = wait_for_import_core_refresh(
            data_root,
            no_daemon,
            selection,
            &self.semantic_completion,
            progress,
        )
        .map_err(|error| {
            if is_terminal_missing_import_path(&error) {
                error.context(ImportPathMissingDuringRefresh)
            } else {
                error
            }
        })?;
        let pinned_generation = refresh.pin.generation_id().to_owned();
        let current_index = refresh.pin.verified_index();
        let current_sessions = current_index.session_count()?;
        let current_searchable_events = current_index.document_count();
        let previous_cardinalities = match refresh.request_previous_generation.as_deref() {
            None => None,
            Some(previous_generation) if previous_generation == pinned_generation => {
                Some((current_sessions, current_searchable_events))
            }
            Some(previous_generation) => {
                // The import publication is already authoritative. A later
                // publication may have retired its predecessor before this
                // optional presentation measurement, so omit the delta
                // instead of failing a successful import or guessing.
                optional_import_baseline(|| {
                    let previous = ctx_history_refresh::pin_retained_generation(
                        data_root,
                        previous_generation,
                    )?;
                    Ok((
                        previous.verified_index().session_count()?,
                        previous.verified_index().document_count(),
                    ))
                })
            }
        };
        let index_facts = ImportIndexFacts::from_cardinalities(
            current_sessions,
            current_searchable_events,
            previous_cardinalities,
        );
        let policy_schema_hash = exact_route_lineages.is_none().then(|| {
            refresh
                .pin
                .verified_index()
                .manifest()
                .policy_schema_hash
                .clone()
        });
        let catalog_content = match (exact_route_lineages, refresh.receipt.as_ref()) {
            (Some(route_lineages), Some(receipt)) => route_lineages
                .into_iter()
                .map(|lineage| {
                    receipt
                        .catalog_route_content(refresh.pin.verified_index(), &lineage)
                        .map(|content| (lineage, content))
                })
                .collect::<Result<_>>()?,
            _ => std::collections::BTreeMap::new(),
        };
        Ok(IngestPublication {
            request_id: refresh.request_id,
            request_previous_generation: refresh.request_previous_generation,
            request_generation_changed: refresh.request_generation_changed,
            scanned_routes: refresh.scanned_routes,
            pinned_generation,
            policy_schema_hash,
            catalog_content,
            index_facts: Some(index_facts),
            receipt: refresh.receipt,
        })
    }
}

fn optional_import_baseline(measure: impl FnOnce() -> Result<(u64, u64)>) -> Option<(u64, u64)> {
    measure().ok()
}

#[cfg(test)]
mod tests {
    use super::optional_import_baseline;

    #[test]
    fn optional_baseline_measurement_cannot_fail_a_successful_import() {
        assert_eq!(
            optional_import_baseline(|| anyhow::bail!("retained generation was reclaimed")),
            None
        );
        assert_eq!(optional_import_baseline(|| Ok((3, 8))), Some((3, 8)));
    }

    #[test]
    fn final_host_contains_only_concrete_side_effect_adapters() {
        let source = include_str!("application_adapter.rs");
        for forbidden in [
            ["Ingest", "Request"].concat(),
            ["Ingest", "RefreshSelection"].concat(),
            ["ImportCore", "RefreshRequest"].concat(),
            ["run", "_ingest"].concat(),
            ["Source", "Discovery", "Port"].concat(),
            ["History", "Cli", "Config"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "final import host contains application authority `{forbidden}`"
            );
        }
    }
}

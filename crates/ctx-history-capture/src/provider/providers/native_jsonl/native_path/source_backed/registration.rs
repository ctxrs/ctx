use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;

use super::{
    hydrate_batch, hydrate_single, DirectJsonlDisposition, DirectJsonlHydrationCatalog,
    DirectJsonlSourceAdapter, DirectJsonlTerminalEvidenceSet,
};
use crate::provider::source_backed::{
    executable_route, invalid_route, route_coordinator_error, route_error,
    source_backed_base_sources, SourceBackedCoordinatorResult, SourceBackedGenerationSink,
    SourceBackedProviderRegistry, SourceBackedRevalidationTarget, SourceBackedRouteDriver,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
use crate::ProviderSource;

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = adapter(source.provider).ok_or_else(|| {
        invalid_route(
            source.provider,
            "provider is not a member of the direct native-JSONL adapter family",
        )
    })?;
    let root = source.path.clone();
    let capture_root = root.clone();
    let source_revalidation_root = root.clone();
    let inventory_revalidation_root = root.clone();
    let hydration_root = root.clone();
    let batch_hydration_root = root;
    let provider = source.provider;
    let certified_source_format = adapter.source_format();
    let terminal_evidence = Arc::new(DirectJsonlTerminalEvidenceSet::default());
    let capture_terminal_evidence = Arc::clone(&terminal_evidence);
    let revalidation_terminal_evidence = Arc::clone(&terminal_evidence);
    let hydration_catalog = Arc::new(Mutex::new(None::<DirectJsonlHydrationCatalog>));
    let single_hydration_catalog = Arc::clone(&hydration_catalog);
    let batch_hydration_catalog = Arc::clone(&hydration_catalog);
    let driver = SourceBackedRouteDriver::new(
        move |sink| capture(adapter, &capture_root, &capture_terminal_evidence, sink),
        move |source| {
            source.provider() == provider.as_str()
                && source.source_format() == certified_source_format
        },
        move |target| {
            revalidate_target(
                adapter,
                &source_revalidation_root,
                &revalidation_terminal_evidence,
                target,
            )
            .unwrap_or(false)
        },
        move |request| hydrate_single(adapter, &hydration_root, &single_hydration_catalog, request),
    )
    .with_batch_hydration(move |request| {
        hydrate_batch(
            adapter,
            &batch_hydration_root,
            &batch_hydration_catalog,
            request,
        )
    })
    .with_complete_inventory_revalidation(move |expected| {
        adapter
            .revalidate_inventory(&inventory_revalidation_root, expected)
            .unwrap_or(false)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn adapter(provider: CaptureProvider) -> Option<DirectJsonlSourceAdapter> {
    match provider {
        CaptureProvider::Antigravity => Some(super::super::antigravity_source_backed_adapter()),
        CaptureProvider::CopilotCli => Some(super::super::copilot_source_backed_adapter()),
        CaptureProvider::FactoryAiDroid => {
            Some(super::super::factory_droid_source_backed_adapter())
        }
        CaptureProvider::Qoder => Some(super::super::qoder_source_backed_adapter()),
        CaptureProvider::QwenCode => Some(super::super::qwen_code_source_backed_adapter()),
        CaptureProvider::Tabnine => Some(super::super::tabnine_source_backed_adapter()),
        CaptureProvider::Windsurf => Some(super::super::windsurf_source_backed_adapter()),
        _ => None,
    }
}

fn capture(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    terminal_evidence: &DirectJsonlTerminalEvidenceSet,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<()> {
    terminal_evidence.reset().map_err(route_error)?;
    let inventory = adapter.discover(root).map_err(route_error)?;
    if inventory.root_missing() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "direct JSONL route root is temporarily unavailable",
        ));
    }
    if !inventory.failures().is_empty() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "direct JSONL inventory contains inaccessible sources",
        ));
    }
    let base_sources = source_backed_base_sources(sink, |source| adapter.owns(source));
    let mut sources = Vec::with_capacity(inventory.leaves().len());
    for leaf in inventory.leaves() {
        let base = base_sources
            .iter()
            .find(|base| adapter.certificate_belongs_to_leaf(leaf, base))
            .cloned();
        let mut reader = adapter
            .open_leaf(leaf, DateTime::<Utc>::UNIX_EPOCH, base.as_ref())
            .map_err(route_error)?;
        let source = reader.source().clone();
        match reader.disposition() {
            DirectJsonlDisposition::Unchanged | DirectJsonlDisposition::Append => {
                let staged_base = sink
                    .begin_source_append(source.clone())
                    .map_err(route_coordinator_error)?
                    .clone();
                if base.as_ref() != Some(&staged_base) {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::SourceChanged,
                        "direct JSONL append base changed before staging",
                    ));
                }
            }
            DirectJsonlDisposition::Cold | DirectJsonlDisposition::Replace => sink
                .begin_source(source.clone())
                .map_err(route_coordinator_error)?,
        }
        reader
            .visit_documents(&mut |document| {
                sink.add_document(document).map_err(|error| {
                    super::DirectJsonlSourceBackedError::Publication(error.to_string())
                })
            })
            .map_err(route_error)?;
        let receipt = reader.finish().map_err(route_error)?;
        terminal_evidence
            .record(receipt.terminal_evidence())
            .map_err(route_error)?;
        sources.push(receipt.source().clone());
        if let Some(append) = receipt.append().cloned() {
            sink.certify_source_append(append)
                .map_err(route_coordinator_error)?;
        } else {
            sink.certify_source(receipt.certificate().clone())
                .map_err(route_coordinator_error)?;
        }
    }
    let closing = adapter.discover(root).map_err(route_error)?;
    let certified_inventory = inventory
        .certify_against(&closing, sources)
        .map_err(route_error)?;
    sink.certify_complete_inventory(certified_inventory.clone())
        .map_err(route_coordinator_error)?;
    for base in base_sources {
        if !certified_inventory.contains(base.observation().source()) {
            let deletion = ctx_history_core::CertifiedSourceDeletion::from_inventory(
                base.observation().source().clone(),
                &certified_inventory,
            )
            .map_err(route_error)?;
            sink.delete_source(deletion, certified_inventory.clone())
                .map_err(route_coordinator_error)?;
        }
    }
    Ok(())
}

fn revalidate_target(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    terminal_evidence: &DirectJsonlTerminalEvidenceSet,
    target: SourceBackedRevalidationTarget<'_>,
) -> super::DirectJsonlSourceBackedResult<bool> {
    match target {
        SourceBackedRevalidationTarget::Source(expected) => {
            adapter.revalidate_certificate(terminal_evidence, expected)
        }
        SourceBackedRevalidationTarget::Deletion(deletion) => {
            adapter.revalidate_deletion(root, deletion)
        }
    }
}

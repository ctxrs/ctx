use std::{path::Path, time::Instant};

use anyhow::{bail, Context, Result};
use ctx_history_capture_model::{
    ProviderImportSummary, ProviderImportWorkResult, ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;
use ctx_history_refresh::{
    ExplicitSourceCatalogUpsert, SourceBackedRefreshReceipt, SourceBackedRefreshRecordRejection,
    SourceBackedRefreshSourceFailure,
};

use crate::diagnostics::{
    classify_import_path_admission_error, classify_import_path_refresh_error,
    classify_owned_import_path_admission_error,
};
use crate::routing::validate_selected_provider;
use crate::{
    automatic_source_preflight, select_history_source_plugin, source_stats,
    validate_ingest_request, AutomaticPublicationOutcome, CaptureAdmissionPort,
    CorePublicationFacts, ExactPublicationOutcome, HistorySourcePluginSource, ImportTotals,
    IngestChange, IngestFailureScope, IngestFailureType, IngestProgressPort, IngestPublication,
    IngestRefreshPort, IngestReport, IngestRequest, IngestRoute, IngestSourceOutcome, IngestStatus,
    IngestTelemetryFacts, IngestTerminalOutcome, PluginPublicationOutcome, ProviderRefreshFacts,
    ProviderRefreshModeFact, RecordRejectionOutcome, RefreshSelection, SourceDiscoveryPort,
    SourceFailureOutcome, SourceStats,
};

const MAX_REPORTED_SOURCE_FAILURES: usize = 3;

pub fn run_ingest<H>(
    request: &IngestRequest,
    data_root: &Path,
    host: &mut H,
) -> Result<IngestReport>
where
    H: SourceDiscoveryPort + CaptureAdmissionPort + IngestRefreshPort + IngestProgressPort,
{
    match validate_ingest_request(request)? {
        IngestRoute::Automatic => run_automatic(request, data_root, host),
        IngestRoute::ExplicitPath => run_exact(request, data_root, host),
        IngestRoute::HistorySourcePlugin => run_plugin(request, data_root, host),
    }
}

fn run_automatic<H>(request: &IngestRequest, data_root: &Path, host: &mut H) -> Result<IngestReport>
where
    H: SourceDiscoveryPort + CaptureAdmissionPort + IngestRefreshPort + IngestProgressPort,
{
    host.begin(0)?;
    let selection = if let Some(provider) = request.provider {
        let report = host.discover_provider(provider)?;
        validate_selected_provider(host, provider, &report)?;
        ctx_history_source_discovery::validate_provider_source_roots_outside_data_root(
            data_root,
            report.sources.iter(),
        )
        .context("validate provider roots before initializing ctx state")?;
        RefreshSelection::Provider(provider)
    } else {
        automatic_source_preflight(host, data_root)
            .context("validate provider roots before initializing ctx state")?;
        RefreshSelection::All
    };

    host.protect_data_root(data_root)
        .context("protect ctx data root before provider refresh")?;
    let publication = host.refresh(data_root, selection, request.no_daemon)?;
    let (publication, receipt) = verified_publication(
        publication,
        "daemon source refresh published without an authoritative terminal receipt",
    )?;
    let scanned_routes = publication
        .scanned_routes
        .context("published daemon source refresh omitted its scanned route count")?;
    let policy_schema_hash = publication
        .policy_schema_hash
        .context("published daemon source refresh omitted its policy schema hash")?;
    let source_failure_total = receipt.source_failure_total();
    let rejected_record_total = receipt.rejected_record_total();
    let sources_completed_with_rejections = receipt
        .route_results
        .iter()
        .filter(|result| result.outcome.is_success() && result.rejected_record_total != 0)
        .count();
    let current = receipt.current;
    let totals = terminal_totals(
        current,
        source_failure_total,
        rejected_record_total,
        sources_completed_with_rejections,
        publication.request_generation_changed,
        publication.index_facts,
    );
    let has_source_failures = source_failure_total != 0;
    let has_rejections = rejected_record_total != 0;
    let source_failures_omitted = receipt.source_failures_omitted().saturating_add(
        receipt
            .source_failure_diagnostic_count()
            .saturating_sub(MAX_REPORTED_SOURCE_FAILURES),
    );
    let rejection_diagnostics_omitted = receipt.rejection_diagnostics_omitted().saturating_add(
        receipt
            .rejection_diagnostic_count()
            .saturating_sub(MAX_REPORTED_SOURCE_FAILURES) as u64,
    );
    let mut sources = vec![IngestSourceOutcome::Automatic(
        AutomaticPublicationOutcome {
            status: if has_source_failures || has_rejections {
                IngestStatus::Partial
            } else {
                IngestStatus::Published
            },
            failure_scope: IngestFailureScope::from_failures(has_source_failures, has_rejections),
            failure_type: IngestFailureType::from_failures(has_source_failures, has_rejections),
            terminal_outcome: IngestTerminalOutcome::from_failures(
                has_source_failures,
                has_rejections,
            ),
            change: change(publication.request_generation_changed),
            previous_generation: publication.request_previous_generation,
            published_generation: receipt.published_generation.clone(),
            generation_changed: publication.request_generation_changed,
            scanned_routes,
            successful_routes: receipt.successful_route_total(),
            source_failure_total,
            source_failures_omitted,
            rejected_record_total,
            sources_completed_with_rejections,
            rejection_diagnostics_reported: receipt
                .rejection_diagnostic_count()
                .min(MAX_REPORTED_SOURCE_FAILURES),
            rejection_diagnostics_omitted,
            current,
            policy_schema_hash,
            request_id: publication.request_id,
        },
    )];
    sources.extend(
        receipt
            .source_failures()
            .take(MAX_REPORTED_SOURCE_FAILURES)
            .cloned()
            .map(source_failure_outcome)
            .map(IngestSourceOutcome::SourceFailure),
    );
    sources.extend(
        receipt
            .rejection_diagnostics()
            .take(MAX_REPORTED_SOURCE_FAILURES)
            .cloned()
            .map(rejection_outcome)
            .map(IngestSourceOutcome::Rejection),
    );
    Ok(IngestReport {
        resume: request.resume,
        totals,
        sources,
        telemetry: None,
        provider_refresh: None,
        core_publication: Some(CorePublicationFacts {
            generation_changed: publication.request_generation_changed,
            source_failure_total,
            rejected_record_total,
        }),
    })
}

fn run_exact<H>(request: &IngestRequest, data_root: &Path, host: &mut H) -> Result<IngestReport>
where
    H: SourceDiscoveryPort + CaptureAdmissionPort + IngestRefreshPort + IngestProgressPort,
{
    let path = request
        .path
        .as_deref()
        .context("explicit source catalog import requires --path")?;
    let source = host
        .explicit_source(data_root, path, request.provider, request.custom_jsonl)
        .map_err(|source| classify_import_path_admission_error(path, source))?;
    if source.status == ProviderSourceStatus::Unsupported {
        return unsupported_source_report(request.resume, &source, host);
    }
    let stats = source_stats(&source.path)
        .map_err(|source_error| {
            classify_owned_import_path_admission_error(path, &source.path, source_error)
        })
        .with_context(|| format!("inspect explicit source {}", source.path.display()))?;
    host.begin(stats.bytes)?;
    host.catalog_exact(&source, stats)?;

    let started = Instant::now();
    let upsert = host
        .admit_exact(data_root, &source, request.relocate_from.as_deref())
        .map_err(|source_error| {
            classify_owned_import_path_admission_error(path, &source.path, source_error)
        })?;
    let publication = host
        .refresh(
            data_root,
            RefreshSelection::ExactSource(upsert.authority.clone()),
            request.no_daemon,
        )
        .map_err(|source| classify_import_path_refresh_error(path, source))?;
    let duration = started.elapsed();
    let (publication, receipt) = verified_publication(
        publication,
        "explicit source refresh has no authoritative terminal receipt",
    )?;
    exact_report(request, upsert, stats, publication, receipt, duration)
}

fn exact_report(
    request: &IngestRequest,
    upsert: ExplicitSourceCatalogUpsert,
    stats: SourceStats,
    publication: IngestPublication,
    receipt: SourceBackedRefreshReceipt,
    duration: std::time::Duration,
) -> Result<IngestReport> {
    let catalog_lineage = upsert.catalog_lineage_hex();
    let requested_outcome = receipt
        .catalog_route_outcome(&catalog_lineage)
        .context("explicit source refresh has no exact catalog-lineage result")?;
    if requested_outcome.outcome == "not_selected" {
        bail!("explicit source refresh did not select its exact catalog route");
    }
    let requested_succeeded = requested_outcome.changed.is_some();
    let requested_failure = receipt
        .source_failures()
        .find(|failure| failure.route_identity == requested_outcome.route_identity)
        .cloned();
    let requested_source_failed = requested_outcome.source_failure_total != 0;
    let requested_rejected = requested_outcome.rejected_record_total != 0;
    let request_content = publication.catalog_content.get(&catalog_lineage).copied();
    let requested_changed = if requested_succeeded && publication.request_generation_changed {
        requested_outcome
            .changed
            .context("successful explicit source route has no change result")?
    } else {
        false
    };
    let rejection_diagnostics = route_rejections(&receipt, &requested_outcome.route_identity);
    let receipt_source_failure_total = receipt.source_failure_total();
    let receipt_rejected_record_total = receipt.rejected_record_total();
    let successful_routes = receipt.successful_route_total();
    let summary = ProviderImportSummary {
        imported: usize::from(requested_succeeded && requested_changed),
        skipped: usize::from(requested_succeeded && !requested_changed),
        ..ProviderImportSummary::default()
    };
    let current = receipt.current;
    let mut totals = ImportTotals {
        per_run_counts_available: false,
        terminal_route_counts_available: true,
        source_files: stats.files,
        source_bytes: stats.bytes,
        imported_sources: usize::from(requested_succeeded),
        sources_completed_with_rejections: usize::from(requested_rejected),
        failed_sources: requested_outcome.source_failure_total,
        failed: usize::try_from(requested_outcome.rejected_record_total).unwrap_or(usize::MAX),
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        request_records_attempted: request_content.map(|content| content.0),
        request_has_usable_records: request_content.map(|content| content.1),
        work_result: if requested_succeeded && requested_changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        },
        ..ImportTotals::default()
    };
    apply_index_facts(&mut totals, publication.index_facts);
    let failure_type = exact_failure_type(
        requested_source_failed,
        requested_rejected,
        requested_outcome.failure_class.as_deref(),
    );
    let source_outcome = ExactPublicationOutcome {
        status: if !requested_succeeded {
            IngestStatus::Failure
        } else if requested_source_failed || requested_rejected {
            IngestStatus::Partial
        } else {
            IngestStatus::Published
        },
        failure_scope: IngestFailureScope::from_failures(
            requested_source_failed,
            requested_rejected,
        ),
        failure_type,
        provider: upsert.provider,
        path: upsert.path,
        source_format: upsert.source_format,
        stats,
        route_identity: requested_outcome.route_identity,
        catalog_lineage,
        request_overlay: upsert.authority,
        previous_generation: publication.request_previous_generation,
        published_generation: receipt.published_generation,
        generation_changed: publication.request_generation_changed,
        scanned_routes: publication
            .scanned_routes
            .context("published daemon source refresh omitted its scanned route count")?,
        successful_routes,
        source_failure_total: receipt_source_failure_total,
        route_source_failure_total: requested_outcome.source_failure_total,
        rejected_record_total: requested_outcome.rejected_record_total,
        rejection_diagnostics,
        request_id: publication.request_id,
        change: change(requested_succeeded && requested_changed),
        current,
        requested_failure: requested_failure.map(source_failure_outcome),
        requested_failure_class: requested_outcome.failure_class,
    };
    Ok(IngestReport {
        resume: request.resume,
        totals,
        sources: vec![IngestSourceOutcome::Exact(source_outcome)],
        telemetry: Some(source_telemetry(
            stats,
            requested_outcome.source_failure_total,
        )),
        provider_refresh: requested_succeeded.then_some(ProviderRefreshFacts {
            provider: upsert.provider,
            mode: if request.custom_jsonl {
                ProviderRefreshModeFact::ExplicitFormat
            } else {
                ProviderRefreshModeFact::ExplicitPath
            },
            summary,
            stats,
            duration,
        }),
        core_publication: Some(CorePublicationFacts {
            generation_changed: publication.request_generation_changed,
            source_failure_total: receipt_source_failure_total,
            rejected_record_total: receipt_rejected_record_total,
        }),
    })
}

fn run_plugin<H>(request: &IngestRequest, data_root: &Path, host: &mut H) -> Result<IngestReport>
where
    H: SourceDiscoveryPort + CaptureAdmissionPort + IngestRefreshPort + IngestProgressPort,
{
    let plugin_source = select_history_source_plugin(
        data_root,
        &request.history_source_manifests,
        request.history_source.as_deref(),
    )?;
    host.begin(0)?;
    host.catalog_plugin(&plugin_source)?;

    let started = Instant::now();
    host.protect_data_root(data_root)
        .context("protect ctx data root before history-source registration")?;
    let route_source = host
        .prepare_plugin(&plugin_source, request.reset_cursor)
        .map_err(|source| {
            if let Some(path) = plugin_source.source_path.as_deref() {
                classify_import_path_admission_error(path, source)
            } else {
                source
            }
        })?;
    let stats = source_stats(&route_source.path).with_context(|| {
        format!(
            "inspect provider-owned history source plugin path {}",
            route_source.path.display()
        )
    })?;
    let upsert = host.admit_exact(data_root, &route_source, None)?;
    let publication = host
        .refresh(
            data_root,
            RefreshSelection::ExactSource(upsert.authority.clone()),
            request.no_daemon,
        )
        .map_err(|source| classify_import_path_refresh_error(&route_source.path, source))?;
    let duration = started.elapsed();
    let (publication, receipt) = verified_publication(
        publication,
        "history source plugin refresh has no authoritative terminal receipt",
    )?;
    plugin_report(
        request,
        plugin_source,
        route_source,
        stats,
        upsert,
        publication,
        receipt,
        duration,
    )
}

#[allow(clippy::too_many_arguments)]
fn plugin_report(
    request: &IngestRequest,
    plugin_source: HistorySourcePluginSource,
    route_source: ctx_history_capture_model::ProviderSource,
    stats: SourceStats,
    upsert: ExplicitSourceCatalogUpsert,
    publication: IngestPublication,
    receipt: SourceBackedRefreshReceipt,
    duration: std::time::Duration,
) -> Result<IngestReport> {
    let catalog_lineage = upsert.catalog_lineage_hex();
    let requested_outcome = receipt
        .catalog_route_outcome(&catalog_lineage)
        .context("history source plugin refresh has no exact catalog-lineage result")?;
    if requested_outcome.changed.is_none() {
        bail!("history source plugin refresh did not publish its exact catalog route");
    }
    let route_changed = publication.request_generation_changed
        && requested_outcome
            .changed
            .context("successful history source plugin route has no change result")?;
    let rejected_record_total = requested_outcome.rejected_record_total;
    let request_content = publication.catalog_content.get(&catalog_lineage).copied();
    let rejection_diagnostics = route_rejections(&receipt, &requested_outcome.route_identity);
    let receipt_source_failure_total = receipt.source_failure_total();
    let receipt_rejected_record_total = receipt.rejected_record_total();
    let summary = ProviderImportSummary {
        imported: usize::from(route_changed),
        skipped: usize::from(!route_changed),
        ..ProviderImportSummary::default()
    };
    let current = receipt.current;
    let mut totals = ImportTotals {
        terminal_route_counts_available: true,
        failed: usize::try_from(rejected_record_total).unwrap_or(usize::MAX),
        sources_completed_with_rejections: usize::from(rejected_record_total != 0),
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        request_records_attempted: request_content.map(|content| content.0),
        request_has_usable_records: request_content.map(|content| content.1),
        work_result: if route_changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        },
        ..ImportTotals::default()
    };
    apply_index_facts(&mut totals, publication.index_facts);
    let has_rejections = rejected_record_total != 0;
    let source_outcome = PluginPublicationOutcome {
        status: if has_rejections {
            IngestStatus::Partial
        } else {
            IngestStatus::Published
        },
        failure_scope: IngestFailureScope::from_failures(false, has_rejections),
        failure_type: IngestFailureType::from_failures(false, has_rejections),
        plugin_source,
        route_source,
        stats,
        catalog_lineage,
        catalog_authority: upsert.authority,
        previous_generation: publication.request_previous_generation,
        published_generation: receipt.published_generation,
        generation_changed: publication.request_generation_changed,
        rejected_record_total,
        rejection_diagnostics,
        request_id: publication.request_id,
        change: change(route_changed),
        current,
    };
    Ok(IngestReport {
        resume: request.resume,
        totals,
        sources: vec![IngestSourceOutcome::Plugin(source_outcome)],
        telemetry: Some(source_telemetry(stats, 0)),
        provider_refresh: Some(ProviderRefreshFacts {
            provider: CaptureProvider::Custom,
            mode: ProviderRefreshModeFact::HistorySourcePlugin,
            summary,
            stats,
            duration,
        }),
        core_publication: Some(CorePublicationFacts {
            generation_changed: publication.request_generation_changed,
            source_failure_total: receipt_source_failure_total,
            rejected_record_total: receipt_rejected_record_total,
        }),
    })
}

fn verified_publication(
    mut publication: IngestPublication,
    missing_receipt: &'static str,
) -> Result<(IngestPublication, SourceBackedRefreshReceipt)> {
    let receipt = publication.receipt.take().context(missing_receipt)?;
    if publication.pinned_generation != receipt.published_generation {
        bail!(
            "Core refresh receipt names generation {}, but the verified publication pin carries {}",
            receipt.published_generation,
            publication.pinned_generation
        );
    }
    Ok((publication, receipt))
}

fn unsupported_source_report<H>(
    resume: bool,
    source: &ctx_history_capture_model::ProviderSource,
    host: &H,
) -> Result<IngestReport>
where
    H: CaptureAdmissionPort,
{
    let detail = source
        .unsupported_reason
        .unwrap_or("the selected provider source is unsupported");
    let source_identity = host
        .source_failure_identity(source)
        .context("derive unsupported source identity")?;
    Ok(IngestReport {
        resume,
        totals: ImportTotals {
            terminal_route_counts_available: true,
            failed_sources: 1,
            work_result: ProviderImportWorkResult::NoOp,
            ..ImportTotals::default()
        },
        sources: vec![IngestSourceOutcome::SourceFailure(SourceFailureOutcome {
            status: IngestStatus::Failure,
            failure_scope: IngestFailureScope::Source,
            failure_type: IngestFailureType::UnsupportedSchema,
            source_identity,
            provider: source.provider.as_str().to_owned(),
            source_failure_class: "incompatible".to_owned(),
            carried_forward: false,
            source_selector: source.path.display().to_string(),
            detail: detail.to_owned(),
        })],
        telemetry: None,
        provider_refresh: None,
        core_publication: None,
    })
}

fn terminal_totals(
    current: ctx_history_refresh::SourceBackedRefreshCurrent,
    source_failure_total: usize,
    rejected_record_total: u64,
    sources_completed_with_rejections: usize,
    generation_changed: bool,
    index_facts: Option<crate::ImportIndexFacts>,
) -> ImportTotals {
    let mut totals = ImportTotals {
        per_run_counts_available: false,
        terminal_route_counts_available: true,
        failed_sources: source_failure_total,
        sources_completed_with_rejections,
        failed: usize::try_from(rejected_record_total).unwrap_or(usize::MAX),
        current_source_count: Some(current.source_count),
        current_indexed_documents: Some(current.indexed_documents),
        current_complete_records: Some(current.complete_records),
        current_retained_records: Some(current.retained_records),
        current_rejected_records: Some(current.rejected_records),
        current_ignored_records: Some(current.ignored_records),
        current_certified_source_bytes: Some(current.certified_source_bytes),
        current_sources_with_rejections: Some(current.sources_with_rejections),
        removed_source_count: Some(current.removed_source_count),
        work_result: if generation_changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        },
        ..ImportTotals::default()
    };
    apply_index_facts(&mut totals, index_facts);
    totals
}

fn apply_index_facts(totals: &mut ImportTotals, facts: Option<crate::ImportIndexFacts>) {
    if let Some(facts) = facts {
        totals.current_indexed_sessions = Some(facts.current_sessions);
        totals.index_delta = facts.delta;
    }
}

fn source_telemetry(stats: SourceStats, failed_sources: usize) -> IngestTelemetryFacts {
    IngestTelemetryFacts {
        sources_seen: 1,
        source_files: stats.files as u64,
        source_bytes: stats.bytes,
        failed_sources: failed_sources as u64,
    }
}

fn route_rejections(
    receipt: &SourceBackedRefreshReceipt,
    route_identity: &str,
) -> Vec<RecordRejectionOutcome> {
    receipt
        .rejection_diagnostics()
        .filter(|rejection| rejection.route_identity == route_identity)
        .cloned()
        .map(rejection_outcome)
        .collect()
}

fn rejection_outcome(rejection: SourceBackedRefreshRecordRejection) -> RecordRejectionOutcome {
    RecordRejectionOutcome {
        source_identity: rejection.source_identity,
        provider: rejection.provider,
        source_selector: rejection.source_selector,
        line: rejection.line,
        payload_type: rejection.payload_type,
        class: rejection.class,
        detail: rejection.detail,
    }
}

fn source_failure_outcome(failure: SourceBackedRefreshSourceFailure) -> SourceFailureOutcome {
    SourceFailureOutcome {
        status: IngestStatus::Failure,
        failure_scope: IngestFailureScope::Source,
        failure_type: if failure.class == "incompatible" {
            IngestFailureType::UnsupportedSchema
        } else {
            IngestFailureType::Other
        },
        source_identity: failure.source_identity,
        provider: failure.provider,
        source_failure_class: failure.class,
        carried_forward: failure.carried_forward,
        source_selector: failure.source_selector,
        detail: failure.detail,
    }
}

fn exact_failure_type(
    source_failed: bool,
    rejected: bool,
    failure_class: Option<&str>,
) -> IngestFailureType {
    match (source_failed, rejected) {
        (false, false) => IngestFailureType::None,
        (false, true) => IngestFailureType::RecordRejection,
        (true, true) => IngestFailureType::RecordRejectionAndSourceFailure,
        (true, false) if failure_class == Some("incompatible") => {
            IngestFailureType::UnsupportedSchema
        }
        (true, false) => IngestFailureType::Other,
    }
}

const fn change(changed: bool) -> IngestChange {
    if changed {
        IngestChange::Changed
    } else {
        IngestChange::NoOp
    }
}

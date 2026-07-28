use std::{
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::{commands::search::RefreshArg, compact_json, SearchBackendArg};

use super::{
    health_search::{
        semantic_filters_need_overfetch, semantic_filters_require_lexical_fallback,
        semantic_hits_for_text_query, semantic_hybrid_coverage_ready,
        semantic_model_cache_available, semantic_query_text, semantic_worker_cache_dir,
    },
    model_contract::SEMANTIC_MODEL_ID,
    paths_status::{
        semantic_vector_path, semantic_worker_report_best_effort, semantic_worker_report_cached,
    },
    reports::{semantic_status_from_worker, SemanticRetrievalReport, SemanticWorkerReport},
    runtime_limits::{SEMANTIC_SEARCH_CANDIDATES, SEMANTIC_SOFT_FILTER_SEARCH_CANDIDATES},
    vector_store::SemanticVectorStore,
    vector_store_schema::SEMANTIC_SQLITE_VEC0_MAX_K,
};

mod transport;
#[cfg(test)]
pub(in crate::semantic) use transport::*;
#[cfg(not(test))]
pub(in crate::semantic) use transport::{
    daemon_query_request, daemon_source_refresh_request, DaemonQueryServiceUnavailable,
    DaemonSourceRefreshServiceUnavailable,
};
mod server;
#[cfg(test)]
pub(in crate::semantic) use server::*;
#[cfg(not(test))]
pub(in crate::semantic) use server::{
    daemon_can_begin_idle_shutdown, observe_daemon_query_activity, start_daemon_query_service,
    start_daemon_source_refresh_service, DaemonQueryActivity, DaemonQueryService,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_packet_with_backend(
    store: &Store,
    data_root: &Path,
    query: &str,
    terms: &[String],
    options: &ctx_history_search::PacketOptions,
    requested_backend: SearchBackendArg,
    semantic_enabled: bool,
    semantic_weight: f32,
    _refresh_mode: RefreshArg,
    emit_warnings: bool,
) -> Result<(ctx_history_search::SearchPacket, SemanticRetrievalReport)> {
    let uses_composed_terms = terms.iter().any(|term| !term.trim().is_empty());
    let semantic_text = semantic_query_text(query, terms);
    let mut effective_backend = requested_backend;

    let filters_require_semantic_fallback =
        matches!(
            effective_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        ) && semantic_filters_require_lexical_fallback(&options.filters);
    let terms_require_semantic_fallback = matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) && uses_composed_terms;
    if filters_require_semantic_fallback && requested_backend == SearchBackendArg::Semantic {
        return Err(anyhow!(
            "semantic search does not yet support these filters; use --backend hybrid or --backend lexical"
        ));
    }
    if terms_require_semantic_fallback && requested_backend == SearchBackendArg::Semantic {
        return Err(anyhow!(
            "semantic search does not yet preserve --term OR semantics; use --backend hybrid or --backend lexical"
        ));
    }
    if filters_require_semantic_fallback || terms_require_semantic_fallback {
        effective_backend = SearchBackendArg::Lexical;
    }

    let lexical_search_packet = || -> Result<ctx_history_search::SearchPacket> {
        if uses_composed_terms {
            ctx_history_search::search_packet_terms(store, query, terms, options)
                .map_err(Into::into)
        } else {
            ctx_history_search::search_packet(store, query, options).map_err(Into::into)
        }
    };

    if !semantic_enabled
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        if requested_backend == SearchBackendArg::Semantic {
            return Err(anyhow!(
                "semantic search is disabled. Set [search] semantic = true in ctx config to enable the local semantic preview"
            ));
        }
        let mut retrieval = SemanticRetrievalReport::lexical(requested_backend, 0);
        retrieval.effective_mode = SearchBackendArg::Lexical;
        retrieval.semantic_weight = 0.0;
        retrieval.semantic_status = "disabled";
        retrieval.set_semantic_fallback(
            "semantic_disabled",
            "local semantic search is disabled by configuration",
        );
        warn_if(
            emit_warnings,
            "warning: local semantic search is disabled; falling back to lexical search",
        );
        return Ok((lexical_search_packet()?, retrieval));
    }

    if !semantic_query_service_supported()
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        if requested_backend == SearchBackendArg::Semantic {
            return Err(anyhow!(
                "local semantic search is not supported on this platform yet"
            ));
        }
        let mut retrieval = SemanticRetrievalReport::lexical(requested_backend, 0);
        retrieval.effective_mode = SearchBackendArg::Lexical;
        retrieval.semantic_weight = 0.0;
        retrieval.semantic_status = "unavailable";
        retrieval.set_semantic_fallback(
            "unsupported_platform",
            "local semantic search is not supported on this platform yet",
        );
        warn_if(
            emit_warnings,
            "warning: local semantic search is not supported on this platform; falling back to lexical search",
        );
        return Ok((lexical_search_packet()?, retrieval));
    }

    let semantic_cache_dir = semantic_worker_cache_dir(data_root);
    let vector_path = semantic_vector_path(data_root);

    let worker_report = if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        semantic_worker_report_cached(data_root, Some(store))?
    } else {
        semantic_worker_report_best_effort(data_root)
    };
    let searchable_items = worker_report.searchable_items;
    let mut retrieval = SemanticRetrievalReport::lexical(requested_backend, searchable_items);
    retrieval.worker = Some(worker_report.clone());
    retrieval.apply_worker_counts(&worker_report);
    if matches!(
        requested_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        retrieval.apply_worker_coverage(&worker_report);
    }

    if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) && semantic_text.trim().is_empty()
    {
        return Err(anyhow!(
            "semantic search needs a text query; add a query or --term"
        ));
    }

    if filters_require_semantic_fallback
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        retrieval.set_semantic_fallback(
            "filtered_vector_lookup_unsupported",
            "semantic search does not yet support filtered vector lookup",
        );
        warn_if(
            emit_warnings,
            "warning: semantic search does not yet support these filters; falling back to lexical search",
        );
    } else if terms_require_semantic_fallback
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        retrieval.set_semantic_fallback(
            "term_or_semantics_unsupported",
            "semantic search does not yet preserve --term OR semantics",
        );
        warn_if(
            emit_warnings,
            "warning: semantic search does not yet preserve --term OR semantics; falling back to lexical search",
        );
    }

    let packet = if matches!(
        effective_backend,
        SearchBackendArg::Semantic | SearchBackendArg::Hybrid
    ) {
        semantic_or_hybrid_search_packet(
            data_root,
            store,
            options,
            &lexical_search_packet,
            &mut retrieval,
            &worker_report,
            &vector_path,
            &semantic_cache_dir,
            &semantic_text,
            effective_backend,
            semantic_weight,
            emit_warnings,
        )?
    } else {
        lexical_search_packet()?
    };

    Ok((packet, retrieval))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn semantic_or_hybrid_search_packet(
    data_root: &Path,
    store: &Store,
    options: &ctx_history_search::PacketOptions,
    lexical_search_packet: &dyn Fn() -> Result<ctx_history_search::SearchPacket>,
    retrieval: &mut SemanticRetrievalReport,
    worker_report: &SemanticWorkerReport,
    vector_path: &Path,
    semantic_cache_dir: &Path,
    semantic_text: &str,
    effective_backend: SearchBackendArg,
    semantic_weight: f32,
    emit_warnings: bool,
) -> Result<ctx_history_search::SearchPacket> {
    match SemanticVectorStore::open_read_only(vector_path) {
        Ok(Some(vector_store)) => {
            *retrieval = SemanticRetrievalReport {
                requested_mode: retrieval.requested_mode,
                effective_mode: effective_backend,
                semantic_weight: if effective_backend == SearchBackendArg::Hybrid {
                    semantic_weight
                } else {
                    1.0
                },
                semantic_status: semantic_status_from_worker(worker_report),
                semantic_fallback_code: None,
                semantic_fallback: None,
                embedding_model: Some(SEMANTIC_MODEL_ID.to_owned()),
                embedded_items: worker_report.embedded_items,
                embedded_chunks: worker_report.embedded_chunks,
                searchable_items: worker_report.searchable_items,
                indexed_now: 0,
                vector_path: Some(vector_path.to_path_buf()),
                worker: Some(worker_report.clone()),
                diagnostics: None,
            };

            if worker_report.embedded_items == 0 {
                if effective_backend == SearchBackendArg::Semantic {
                    if !worker_report.model_cache_available
                        || !semantic_model_cache_available(semantic_cache_dir)
                    {
                        return Err(anyhow!(
                            "semantic index has no embedded event chunks and semantic model is not available in the local cache; semantic-only search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                        ));
                    }
                    return Err(anyhow!(
                        "semantic index has no embedded event chunks yet; ctx search does not start semantic indexing"
                    ));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "semantic_index_empty",
                    "semantic index has no embedded event chunks",
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic index is empty; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if effective_backend == SearchBackendArg::Hybrid
                && (!worker_report.searchable_items_known
                    || !semantic_hybrid_coverage_ready(
                        worker_report.embedded_items,
                        worker_report.searchable_items,
                        worker_report.dirty_items,
                    ))
            {
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                if worker_report.searchable_items_known {
                    retrieval.set_semantic_fallback(
                        "semantic_coverage_not_ready",
                        format!(
                            "semantic coverage is incomplete or dirty for hybrid ranking ({}/{} items embedded, {} dirty)",
                            worker_report.embedded_items,
                            worker_report.searchable_items,
                            worker_report.dirty_items
                        ),
                    );
                } else {
                    retrieval.set_semantic_fallback(
                        "semantic_coverage_unknown",
                        "semantic coverage is not cached yet; wait for the daemon to refresh indexing status",
                    );
                }
                warn_if(
                    emit_warnings,
                    "warning: semantic coverage is incomplete or dirty for hybrid ranking; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            if !worker_report.model_cache_available
                || !semantic_model_cache_available(semantic_cache_dir)
            {
                if effective_backend == SearchBackendArg::Semantic {
                    return Err(anyhow!(
                        "semantic model is not available in the local cache; semantic-only search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                    ));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.set_semantic_fallback(
                    "model_cache_missing",
                    "semantic model is not available in the local cache",
                );
                warn_if(
                    emit_warnings,
                    "warning: semantic model is not available in the local cache; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            let query_service = daemon_query_service_ping(data_root).and_then(|available| {
                if available {
                    Ok(())
                } else {
                    Err(DaemonQueryServiceUnavailable.into())
                }
            });
            if let Err(error) = query_service {
                let unavailable = error
                    .downcast_ref::<DaemonQueryServiceUnavailable>()
                    .is_some();
                let message = if unavailable {
                    DaemonQueryServiceUnavailable.to_string()
                } else {
                    format!("daemon semantic query service check failed: {error:#}")
                };
                if effective_backend == SearchBackendArg::Semantic {
                    return Err(error.context("semantic search failed"));
                }
                retrieval.effective_mode = SearchBackendArg::Lexical;
                retrieval.semantic_weight = 0.0;
                retrieval.embedding_model = None;
                retrieval.semantic_status = "unavailable";
                retrieval.set_semantic_fallback(
                    if unavailable {
                        "daemon_query_service_unavailable"
                    } else {
                        "semantic_retrieval_failed"
                    },
                    message,
                );
                warn_if(
                    emit_warnings,
                    "warning: daemon semantic query service is not available; falling back to lexical search",
                );
                return lexical_search_packet();
            }

            let candidate_limit = semantic_candidate_limit(options);
            match semantic_hits_for_text_query(
                data_root,
                store,
                &vector_store,
                semantic_text,
                candidate_limit,
            ) {
                Ok((semantic_hits, diagnostics)) => {
                    retrieval.diagnostics = Some(diagnostics);
                    ctx_history_search::semantic_event_search_packet(
                        store,
                        semantic_text,
                        options,
                        &semantic_hits,
                        semantic_weight,
                        effective_backend == SearchBackendArg::Hybrid,
                    )
                    .map_err(Into::into)
                }
                Err(error) => {
                    let unavailable = error
                        .downcast_ref::<DaemonQueryServiceUnavailable>()
                        .is_some();
                    let error_message = if unavailable {
                        DaemonQueryServiceUnavailable.to_string()
                    } else {
                        format!("{error:#}")
                    };
                    if effective_backend == SearchBackendArg::Semantic {
                        return Err(error.context("semantic search failed"));
                    }
                    retrieval.effective_mode = SearchBackendArg::Lexical;
                    retrieval.semantic_weight = 0.0;
                    retrieval.embedding_model = None;
                    retrieval.semantic_status = "unavailable";
                    retrieval.diagnostics = None;
                    if unavailable {
                        retrieval.set_semantic_fallback(
                            "daemon_query_service_unavailable",
                            error_message,
                        );
                    } else {
                        retrieval.set_semantic_fallback(
                            "semantic_retrieval_failed",
                            format!("semantic retrieval failed: {error_message}"),
                        );
                    }
                    warn_if(
                        emit_warnings,
                        "warning: semantic retrieval failed; falling back to lexical search",
                    );
                    lexical_search_packet()
                }
            }
        }
        Ok(None) => {
            if effective_backend == SearchBackendArg::Semantic {
                if !worker_report.model_cache_available
                    || !semantic_model_cache_available(semantic_cache_dir)
                {
                    return Err(anyhow!(
                        "semantic index is not available yet and semantic model is not available in the local cache; semantic-only search will not initialize or download {SEMANTIC_MODEL_ID} during search"
                    ));
                }
                return Err(anyhow!(
                    "semantic index is not available yet; ctx search does not start semantic indexing"
                ));
            }
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.set_semantic_fallback(
                "semantic_index_missing",
                "semantic index is not available yet",
            );
            warn_if(
                emit_warnings,
                "warning: semantic index is not available yet; falling back to lexical search",
            );
            lexical_search_packet()
        }
        Err(error) => {
            let message = format!("semantic index could not be opened: {error:#}");
            if effective_backend == SearchBackendArg::Semantic {
                return Err(anyhow!(message));
            }
            retrieval.effective_mode = SearchBackendArg::Lexical;
            retrieval.semantic_weight = 0.0;
            retrieval.embedding_model = None;
            retrieval.semantic_status = "unavailable";
            retrieval.set_semantic_fallback("semantic_index_open_error", message);
            warn_if(
                emit_warnings,
                "warning: semantic index could not be opened; falling back to lexical search",
            );
            lexical_search_packet()
        }
    }
}

pub(super) fn semantic_candidate_limit(options: &ctx_history_search::PacketOptions) -> usize {
    let overfetch = if semantic_filters_need_overfetch(&options.filters) {
        SEMANTIC_SOFT_FILTER_SEARCH_CANDIDATES.max(options.limit.saturating_mul(100))
    } else {
        SEMANTIC_SEARCH_CANDIDATES.max(options.limit.saturating_mul(8))
    };
    overfetch.min(SEMANTIC_SQLITE_VEC0_MAX_K)
}

pub(super) fn warn_if(enabled: bool, message: &str) {
    if enabled {
        eprintln!("{message}");
    }
}

pub(crate) fn semantic_query_service_supported() -> bool {
    cfg!(ctx_semantic_fastembed)
}

pub(in crate::semantic) fn daemon_query_service_transport_supported() -> bool {
    cfg!(any(unix, windows))
}

pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    daemon_query_service_ping(data_root).unwrap_or(false)
}

fn daemon_query_service_ping(data_root: &Path) -> Result<bool> {
    let response = daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(1),
        1024,
    )?;
    Ok(response
        .as_ref()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        == Some(true))
}

pub(crate) fn wait_for_daemon_query_service(data_root: &Path, timeout: StdDuration) -> bool {
    if !semantic_query_service_supported() {
        return false;
    }
    let started = Instant::now();
    loop {
        if daemon_query_service_available(data_root) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(StdDuration::from_millis(100));
    }
}

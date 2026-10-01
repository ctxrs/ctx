use ctx_client_observability::analytics as wire;
use ctx_graph::ctx_graph_core::observation as native;

use super::graph_vocabulary as vocabulary;

/// Lifecycle milestones are not completed tool calls. An abandoned request
/// without an execution result cannot truthfully become a success or failure.
pub(crate) fn graph(facts: native::GraphObservation) -> Option<wire::PublicEventV1> {
    if let Some(lifecycle) = facts.lifecycle {
        let kind = match facts.operation {
            native::GraphOperation::Serve => wire::ProductRuntimeKind::GraphServe,
            native::GraphOperation::Watch => wire::ProductRuntimeKind::GraphWatch,
            _ => return None,
        };
        let phase = match lifecycle {
            native::GraphLifecycle::Ready => wire::ProductRuntimePhase::Ready,
            native::GraphLifecycle::Stopped if facts.failure.is_none() => {
                wire::ProductRuntimePhase::Stopped
            }
            native::GraphLifecycle::Stopped | native::GraphLifecycle::StartFailed => {
                wire::ProductRuntimePhase::Failed
            }
        };
        let mut event = wire::ProductRuntimeV1::new(kind, phase, facts.duration?);
        event.failure = facts.failure.map(failure);
        return Some(event.into_event());
    }
    graph_completed(facts).map(wire::GraphCompletedV1::into_event)
}

pub(crate) fn graph_completed(facts: native::GraphObservation) -> Option<wire::GraphCompletedV1> {
    let execution = if let Some(failure_facts) = facts.failure {
        Err(failure(failure_facts))
    } else if facts.execution_succeeded? {
        Ok(())
    } else {
        Err(wire::ProductFailure {
            stage: stage(facts.phase),
            class: wire::ProductFailureClass::Other,
        })
    };
    let mcp = matches!(
        facts.invocation,
        native::GraphInvocation::UnifiedMcp
            | native::GraphInvocation::NativeStdio
            | native::GraphInvocation::NativeHttp
    );
    // Yielding an HTTP body proves handoff, not completed client delivery.
    let delivery = match (facts.output_served, facts.output_boundary) {
        (Some(false), _) => wire::DeliveryEvidence::Failed,
        (
            Some(true),
            native::GraphOutputBoundary::CliFlush | native::GraphOutputBoundary::StdioFlush,
        ) => wire::DeliveryEvidence::KnownComplete,
        _ => wire::DeliveryEvidence::Unknown,
    };
    let mut completion = wire::ProductCompletion::new(
        facts.duration?,
        execution,
        delivery,
        if mcp {
            wire::ProductOutput::Mcp
        } else {
            wire::ProductOutput::Bytes
        },
    );
    completion.timings.work = facts.query_duration;
    completion.timings.output = facts.output_duration;
    let mut event = wire::GraphCompletedV1::new(
        vocabulary::operation(facts.operation),
        if mcp {
            wire::GraphSurface::Mcp
        } else {
            wire::GraphSurface::Cli
        },
        completion,
    );
    event.nodes = facts.nodes;
    event.edges = facts.edges;
    event.files_processed = facts.parsed_files;
    event.result = facts.result_count.map(|count| wire::ProductResultFacts {
        count,
        truncated: facts.truncated,
    });
    event.details = wire::GraphDetails {
        invocation: Some(vocabulary::invocation(facts.invocation)),
        phase: Some(vocabulary::phase(facts.phase)),
        failure: facts.failure.map(|f| wire::GraphFailureFacts {
            phase: vocabulary::phase(f.phase),
            kind: vocabulary::failure_kind(f.kind),
        }),
        output_boundary: Some(vocabulary::output_boundary(facts.output_boundary)),
        query: Some(wire::GraphQueryFacts {
            bounds: facts.bounds.map(|b| wire::GraphBounds {
                seed: b.seed,
                node: b.node,
                work: b.work,
                depth: b.depth,
                unresolved: b.unresolved,
                token: b.token,
                other: b.other,
            }),
            path: facts.path.map(vocabulary::path_disposition),
            unresolved: facts.unresolved,
            seeds: facts.seeds,
            duration: facts.query_duration,
        }),
        index: Some(wire::GraphIndexFacts {
            disposition: facts.index.map(vocabulary::index_disposition),
            fresh: facts.fresh,
            parsed: facts.parsed_files,
            rejected: facts.rejected_files,
            unchanged: facts.unchanged_files,
            deleted: facts.deleted_files,
            diagnostics: facts.diagnostics,
            capture: facts.capture_duration,
            detect: facts.detect_duration,
            extract: facts.extract_duration,
            commit: facts.commit_duration,
        }),
        analysis: Some(wire::GraphAnalysisFacts {
            algorithm: facts.algorithm.map(vocabulary::algorithm),
            communities: facts.communities,
            pagerank_converged: facts.pagerank_converged,
            convergence: facts.community_convergence.map(vocabulary::convergence),
            passes: facts.community_passes,
            unsatisfied_constraints: facts.unsatisfied_constraints,
            duration: facts.analysis_duration,
        }),
        artifact: Some(wire::GraphArtifactFacts {
            snapshot_cache_hit: facts.snapshot_cache_hit,
            analysis_cache_hit: facts.analysis_cache_hit,
            committed: facts.artifact_committed,
            bytes: facts.artifact_bytes,
            format: facts.export_format.map(vocabulary::export_format),
        }),
        semantic: semantic(facts.semantic),
        polls: facts.polls,
        retries: facts.retries,
    };
    Some(event)
}

fn semantic(facts: native::GraphSemanticFacts) -> Option<wire::GraphSemanticFacts> {
    if facts == native::GraphSemanticFacts::default() {
        return None;
    }
    let tokens = |f: native::GraphTokenUsage| wire::GraphTokenUsage {
        known_sum: f.known_sum,
        reporting_receipts: f.reporting_receipts,
    };
    Some(wire::GraphSemanticFacts {
        configured: facts.configured,
        reserved_generations: facts.reserved_generations,
        reserved_output_tokens: facts.reserved_output_tokens,
        receipts: facts.receipts,
        usage_unavailable: facts.usage_unavailable,
        input: tokens(facts.input),
        output: tokens(facts.output),
        total: tokens(facts.total),
        cache_read: tokens(facts.cache_read),
        cache_create: tokens(facts.cache_create),
        reasoning: tokens(facts.reasoning),
    })
}

fn failure(facts: native::GraphFailure) -> wire::ProductFailure {
    use native::GraphFailureKind as N;
    use wire::ProductFailureClass as W;
    wire::ProductFailure {
        stage: stage(facts.phase),
        class: match facts.kind {
            N::InvalidInput | N::InputRejected | N::EndpointAmbiguous | N::Protocol => {
                W::InvalidRequest
            }
            N::MissingIndex | N::NotFound | N::EndpointNotFound | N::UnknownProject => W::NotFound,
            N::Permission => W::Permission,
            N::Io | N::BrokenPipe => W::Io,
            N::StoreOpen | N::StoreBusy | N::InvalidStore => W::Store,
            N::UnsupportedStore => W::Unsupported,
            N::ConcurrentChange => W::Conflict,
            N::WorkLimit | N::ResponseLimit | N::Capacity => W::Capacity,
            N::Authentication => W::Unauthorized,
            N::Worker | N::Serialize | N::Unknown => W::Other,
        },
    }
}

fn stage(phase: native::GraphPhase) -> wire::ProductFailureStage {
    use native::GraphPhase as N;
    use wire::ProductFailureStage as W;
    match phase {
        N::Parse | N::Protocol | N::Admission => W::Parse,
        N::Prepare | N::Discover | N::Open | N::Registration | N::Bind => W::Prepare,
        N::Render => W::Render,
        N::OutputWrite | N::OutputFlush => W::Output,
        N::Capture
        | N::Detect
        | N::Extract
        | N::Commit
        | N::PostCommit
        | N::Query
        | N::Snapshot
        | N::Analysis
        | N::ArtifactWrite
        | N::Worker
        | N::Shutdown => W::Execute,
    }
}

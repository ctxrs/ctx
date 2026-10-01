//! Exhaustive projections into the collector-owned closed vocabulary.
use ctx_client_observability::analytics as wire;
use ctx_graph::ctx_graph_core::observation as native;

pub(super) fn operation(value: native::GraphOperation) -> wire::GraphOperation {
    match value {
        native::GraphOperation::Parse => wire::GraphOperation::Parse,
        native::GraphOperation::Index => wire::GraphOperation::Index,
        native::GraphOperation::Update => wire::GraphOperation::Update,
        native::GraphOperation::Watch => wire::GraphOperation::Watch,
        native::GraphOperation::CheckUpdate => wire::GraphOperation::CheckUpdate,
        native::GraphOperation::Add => wire::GraphOperation::Add,
        native::GraphOperation::Clone => wire::GraphOperation::Clone,
        native::GraphOperation::Import => wire::GraphOperation::Import,
        native::GraphOperation::Compact => wire::GraphOperation::Compact,
        native::GraphOperation::Search => wire::GraphOperation::Search,
        native::GraphOperation::Show => wire::GraphOperation::Show,
        native::GraphOperation::Callers => wire::GraphOperation::Callers,
        native::GraphOperation::Callees => wire::GraphOperation::Callees,
        native::GraphOperation::Impact => wire::GraphOperation::Impact,
        native::GraphOperation::Path => wire::GraphOperation::Path,
        native::GraphOperation::Stats => wire::GraphOperation::Stats,
        native::GraphOperation::Analyze => wire::GraphOperation::Analyze,
        native::GraphOperation::Communities => wire::GraphOperation::Communities,
        native::GraphOperation::Hubs => wire::GraphOperation::Hubs,
        native::GraphOperation::Tree => wire::GraphOperation::Tree,
        native::GraphOperation::Export => wire::GraphOperation::Export,
        native::GraphOperation::Report => wire::GraphOperation::Report,
        native::GraphOperation::Diagnose => wire::GraphOperation::Diagnose,
        native::GraphOperation::Benchmark => wire::GraphOperation::Benchmark,
        native::GraphOperation::Label => wire::GraphOperation::Label,
        native::GraphOperation::Merge => wire::GraphOperation::Merge,
        native::GraphOperation::GlobalAdd => wire::GraphOperation::GlobalAdd,
        native::GraphOperation::GlobalRemove => wire::GraphOperation::GlobalRemove,
        native::GraphOperation::GlobalList => wire::GraphOperation::GlobalList,
        native::GraphOperation::GlobalRefresh => wire::GraphOperation::GlobalRefresh,
        native::GraphOperation::GlobalSearch => wire::GraphOperation::GlobalSearch,
        native::GraphOperation::GlobalPath => wire::GraphOperation::GlobalPath,
        native::GraphOperation::SaveResult => wire::GraphOperation::SaveResult,
        native::GraphOperation::Reflect => wire::GraphOperation::Reflect,
        native::GraphOperation::Prs => wire::GraphOperation::Prs,
        native::GraphOperation::Install => wire::GraphOperation::Install,
        native::GraphOperation::Uninstall => wire::GraphOperation::Uninstall,
        native::GraphOperation::Hook => wire::GraphOperation::Hook,
        native::GraphOperation::HookGuard => wire::GraphOperation::HookGuard,
        native::GraphOperation::Switch => wire::GraphOperation::Switch,
        native::GraphOperation::ProviderList => wire::GraphOperation::ProviderList,
        native::GraphOperation::ProviderDetect => wire::GraphOperation::ProviderDetect,
        native::GraphOperation::ProviderTemplate => wire::GraphOperation::ProviderTemplate,
        native::GraphOperation::ProviderSetup => wire::GraphOperation::ProviderSetup,
        native::GraphOperation::ProviderShow => wire::GraphOperation::ProviderShow,
        native::GraphOperation::ProviderAdd => wire::GraphOperation::ProviderAdd,
        native::GraphOperation::ProviderRemove => wire::GraphOperation::ProviderRemove,
        native::GraphOperation::CacheInspect => wire::GraphOperation::CacheInspect,
        native::GraphOperation::CacheRemove => wire::GraphOperation::CacheRemove,
        native::GraphOperation::IntrospectPostgres => wire::GraphOperation::IntrospectPostgres,
        native::GraphOperation::PushNeo4j => wire::GraphOperation::PushNeo4j,
        native::GraphOperation::PushFalkorDb => wire::GraphOperation::PushFalkorDb,
        native::GraphOperation::Serve => wire::GraphOperation::Serve,
        native::GraphOperation::QueryGraph => wire::GraphOperation::QueryGraph,
        native::GraphOperation::GetNode => wire::GraphOperation::GetNode,
        native::GraphOperation::GetNeighbors => wire::GraphOperation::GetNeighbors,
        native::GraphOperation::ShortestPath => wire::GraphOperation::ShortestPath,
        native::GraphOperation::GraphStats => wire::GraphOperation::GraphStats,
        native::GraphOperation::GodNodes => wire::GraphOperation::GodNodes,
        native::GraphOperation::GetCommunity => wire::GraphOperation::GetCommunity,
        native::GraphOperation::ListPrs => wire::GraphOperation::ListPrs,
        native::GraphOperation::GetPrImpact => wire::GraphOperation::GetPrImpact,
        native::GraphOperation::TriagePrs => wire::GraphOperation::TriagePrs,
        native::GraphOperation::ResourceStats => wire::GraphOperation::ResourceStats,
        native::GraphOperation::ResourceGraph => wire::GraphOperation::ResourceGraph,
        native::GraphOperation::ResourceReport => wire::GraphOperation::ResourceReport,
        native::GraphOperation::ResourceHubs => wire::GraphOperation::ResourceHubs,
        native::GraphOperation::ResourceCommunities => wire::GraphOperation::ResourceCommunities,
        native::GraphOperation::ResourceComputedCommunities => {
            wire::GraphOperation::ResourceComputedCommunities
        }
        native::GraphOperation::ResourceSurprises => wire::GraphOperation::ResourceSurprises,
        native::GraphOperation::ResourceAudit => wire::GraphOperation::ResourceAudit,
        native::GraphOperation::ResourceQuestions => wire::GraphOperation::ResourceQuestions,
        native::GraphOperation::Initialize => wire::GraphOperation::Initialize,
        native::GraphOperation::ToolsList => wire::GraphOperation::ToolsList,
        native::GraphOperation::ResourcesList => wire::GraphOperation::ResourcesList,
        native::GraphOperation::Ping => wire::GraphOperation::Ping,
        native::GraphOperation::Protocol => wire::GraphOperation::Protocol,
    }
}

pub(super) fn invocation(value: native::GraphInvocation) -> wire::GraphInvocation {
    match value {
        native::GraphInvocation::Cli => wire::GraphInvocation::Cli,
        native::GraphInvocation::ScopedSearch => wire::GraphInvocation::ScopedSearch,
        native::GraphInvocation::UnifiedMcp => wire::GraphInvocation::UnifiedMcp,
        native::GraphInvocation::NativeStdio => wire::GraphInvocation::NativeStdio,
        native::GraphInvocation::NativeHttp => wire::GraphInvocation::NativeHttp,
        native::GraphInvocation::Library => wire::GraphInvocation::Library,
    }
}

pub(super) fn phase(value: native::GraphPhase) -> wire::GraphPhase {
    match value {
        native::GraphPhase::Parse => wire::GraphPhase::Parse,
        native::GraphPhase::Prepare => wire::GraphPhase::Prepare,
        native::GraphPhase::Discover => wire::GraphPhase::Discover,
        native::GraphPhase::Open => wire::GraphPhase::Open,
        native::GraphPhase::Capture => wire::GraphPhase::Capture,
        native::GraphPhase::Detect => wire::GraphPhase::Detect,
        native::GraphPhase::Extract => wire::GraphPhase::Extract,
        native::GraphPhase::Commit => wire::GraphPhase::Commit,
        native::GraphPhase::PostCommit => wire::GraphPhase::PostCommit,
        native::GraphPhase::Query => wire::GraphPhase::Query,
        native::GraphPhase::Snapshot => wire::GraphPhase::Snapshot,
        native::GraphPhase::Analysis => wire::GraphPhase::Analysis,
        native::GraphPhase::Render => wire::GraphPhase::Render,
        native::GraphPhase::ArtifactWrite => wire::GraphPhase::ArtifactWrite,
        native::GraphPhase::OutputWrite => wire::GraphPhase::OutputWrite,
        native::GraphPhase::OutputFlush => wire::GraphPhase::OutputFlush,
        native::GraphPhase::Registration => wire::GraphPhase::Registration,
        native::GraphPhase::Bind => wire::GraphPhase::Bind,
        native::GraphPhase::Protocol => wire::GraphPhase::Protocol,
        native::GraphPhase::Admission => wire::GraphPhase::Admission,
        native::GraphPhase::Worker => wire::GraphPhase::Worker,
        native::GraphPhase::Shutdown => wire::GraphPhase::Shutdown,
    }
}

pub(super) fn failure_kind(value: native::GraphFailureKind) -> wire::GraphFailureKind {
    match value {
        native::GraphFailureKind::InvalidInput => wire::GraphFailureKind::InvalidInput,
        native::GraphFailureKind::MissingIndex => wire::GraphFailureKind::MissingIndex,
        native::GraphFailureKind::NotFound => wire::GraphFailureKind::NotFound,
        native::GraphFailureKind::Permission => wire::GraphFailureKind::Permission,
        native::GraphFailureKind::Io => wire::GraphFailureKind::Io,
        native::GraphFailureKind::StoreOpen => wire::GraphFailureKind::StoreOpen,
        native::GraphFailureKind::StoreBusy => wire::GraphFailureKind::StoreBusy,
        native::GraphFailureKind::InvalidStore => wire::GraphFailureKind::InvalidStore,
        native::GraphFailureKind::UnsupportedStore => wire::GraphFailureKind::UnsupportedStore,
        native::GraphFailureKind::ConcurrentChange => wire::GraphFailureKind::ConcurrentChange,
        native::GraphFailureKind::EndpointNotFound => wire::GraphFailureKind::EndpointNotFound,
        native::GraphFailureKind::EndpointAmbiguous => wire::GraphFailureKind::EndpointAmbiguous,
        native::GraphFailureKind::WorkLimit => wire::GraphFailureKind::WorkLimit,
        native::GraphFailureKind::ResponseLimit => wire::GraphFailureKind::ResponseLimit,
        native::GraphFailureKind::InputRejected => wire::GraphFailureKind::InputRejected,
        native::GraphFailureKind::UnknownProject => wire::GraphFailureKind::UnknownProject,
        native::GraphFailureKind::Capacity => wire::GraphFailureKind::Capacity,
        native::GraphFailureKind::Worker => wire::GraphFailureKind::Worker,
        native::GraphFailureKind::Serialize => wire::GraphFailureKind::Serialize,
        native::GraphFailureKind::BrokenPipe => wire::GraphFailureKind::BrokenPipe,
        native::GraphFailureKind::Authentication => wire::GraphFailureKind::Authentication,
        native::GraphFailureKind::Protocol => wire::GraphFailureKind::Protocol,
        native::GraphFailureKind::Unknown => wire::GraphFailureKind::Unknown,
    }
}

pub(super) fn path_disposition(value: native::GraphPathDisposition) -> wire::GraphPathDisposition {
    match value {
        native::GraphPathDisposition::Found => wire::GraphPathDisposition::Found,
        native::GraphPathDisposition::NotFoundWithinScope => {
            wire::GraphPathDisposition::NotFoundWithinScope
        }
        native::GraphPathDisposition::Incomplete => wire::GraphPathDisposition::Incomplete,
    }
}

pub(super) fn index_disposition(
    value: native::GraphIndexDisposition,
) -> wire::GraphIndexDisposition {
    match value {
        native::GraphIndexDisposition::NoOp => wire::GraphIndexDisposition::NoOp,
        native::GraphIndexDisposition::Committed => wire::GraphIndexDisposition::Committed,
    }
}

pub(super) fn convergence(value: native::GraphConvergence) -> wire::GraphConvergence {
    match value {
        native::GraphConvergence::Converged => wire::GraphConvergence::Converged,
        native::GraphConvergence::NotConverged => wire::GraphConvergence::NotConverged,
        native::GraphConvergence::Unknown => wire::GraphConvergence::Unknown,
    }
}

pub(super) fn algorithm(value: native::GraphAlgorithm) -> wire::GraphAlgorithm {
    match value {
        native::GraphAlgorithm::Leiden => wire::GraphAlgorithm::Leiden,
        native::GraphAlgorithm::Louvain => wire::GraphAlgorithm::Louvain,
    }
}

pub(super) fn output_boundary(value: native::GraphOutputBoundary) -> wire::GraphOutputBoundary {
    match value {
        native::GraphOutputBoundary::Unobserved => wire::GraphOutputBoundary::Unobserved,
        native::GraphOutputBoundary::CliFlush => wire::GraphOutputBoundary::CliFlush,
        native::GraphOutputBoundary::StdioFlush => wire::GraphOutputBoundary::StdioFlush,
        native::GraphOutputBoundary::HttpBody => wire::GraphOutputBoundary::HttpBody,
    }
}

pub(super) fn export_format(value: native::GraphExportFormat) -> wire::GraphExportFormat {
    match value {
        native::GraphExportFormat::SnapshotJson => wire::GraphExportFormat::SnapshotJson,
        native::GraphExportFormat::GraphifyJson => wire::GraphExportFormat::GraphifyJson,
        native::GraphExportFormat::GraphMl => wire::GraphExportFormat::GraphMl,
        native::GraphExportFormat::Cypher => wire::GraphExportFormat::Cypher,
        native::GraphExportFormat::Mermaid => wire::GraphExportFormat::Mermaid,
        native::GraphExportFormat::Svg => wire::GraphExportFormat::Svg,
        native::GraphExportFormat::Html => wire::GraphExportFormat::Html,
        native::GraphExportFormat::Markdown => wire::GraphExportFormat::Markdown,
        native::GraphExportFormat::Canvas => wire::GraphExportFormat::Canvas,
        native::GraphExportFormat::CallflowHtml => wire::GraphExportFormat::CallflowHtml,
        native::GraphExportFormat::TreeHtml => wire::GraphExportFormat::TreeHtml,
        native::GraphExportFormat::Wiki => wire::GraphExportFormat::Wiki,
        native::GraphExportFormat::Obsidian => wire::GraphExportFormat::Obsidian,
    }
}

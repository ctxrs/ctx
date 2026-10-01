use super::wire::{completion, count, result};
use super::*;
use serde_json::json;

vocabulary!(RemoteOperation { Search => "search", Event => "event", Session => "session", Status => "status", Unsupported => "unsupported" });
pub type RemoteSurface = GraphSurface;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoteReadFacts {
    pub page_count: Option<u64>,
    pub limit: Option<u64>,
    pub client_limited: Option<bool>,
    pub complete: Option<bool>,
    pub exhaustive: Option<bool>,
    pub has_more: Option<bool>,
    pub response_limited: Option<bool>,
    pub coverage_lag: Option<u64>,
    pub query_duration: Option<Duration>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteCompletedV1 {
    pub operation: RemoteOperation,
    pub surface: RemoteSurface,
    pub completion: ProductCompletion,
    pub result: Option<ProductResultFacts>,
    pub read: RemoteReadFacts,
}
impl RemoteCompletedV1 {
    pub fn new(
        operation: RemoteOperation,
        surface: RemoteSurface,
        completion: ProductCompletion,
    ) -> Self {
        Self {
            operation,
            surface,
            completion,
            result: None,
            read: RemoteReadFacts::default(),
        }
    }
    pub fn into_event(self) -> PublicEventV1 {
        PublicEventV1::RemoteCompleted(self)
    }
}
pub(super) fn wire(e: &RemoteCompletedV1) -> EngineWire {
    let mut p = completion(e.completion);
    p.insert("remote_operation".into(), json!(e.operation.as_str()));
    result(&mut p, e.result);
    for (key, v) in [
        ("remote_page_count_bucket", e.read.page_count),
        ("remote_limit_count_bucket", e.read.limit),
        ("remote_coverage_lag_count_bucket", e.read.coverage_lag),
    ] {
        count(&mut p, key, v);
    }
    for (key, v) in [
        ("remote_client_limited", e.read.client_limited),
        ("remote_complete", e.read.complete),
        ("remote_exhaustive", e.read.exhaustive),
        ("remote_has_more", e.read.has_more),
        ("remote_response_limited", e.read.response_limited),
    ] {
        if let Some(v) = v {
            p.insert(key.into(), json!(v));
        }
    }
    if let Some(v) = e.read.query_duration {
        p.insert(
            "remote_query_duration_bucket".into(),
            json!(native_duration_bucket(v)),
        );
    }
    (
        "operation_completed",
        match e.surface {
            RemoteSurface::Cli => Surface::Cli,
            RemoteSurface::Mcp => Surface::Mcp,
        },
        "remote",
        e.completion.outcome(),
        duration_bucket(e.completion.elapsed),
        p,
    )
}

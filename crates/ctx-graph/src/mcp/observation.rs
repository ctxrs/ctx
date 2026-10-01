//! Bounded request facts attached to the SDK's existing request and transport objects.
use super::*;
use crate::observation::*;
use futures_util::{Sink, Stream};
use rmcp::model::{ClientNotification, ClientRequest, GetExtensions, JsonRpcMessage, RequestId};
use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::Instant,
};

#[derive(Clone)]
pub(super) struct RequestObservation(Arc<RequestState>);
struct RequestState {
    facts: Mutex<Option<GraphObservation>>,
    started: Instant,
    observer: GraphObserver,
}
impl Drop for RequestState {
    fn drop(&mut self) {
        if let Ok(slot) = self.facts.get_mut()
            && let Some(mut facts) = slot.take()
        {
            // Cancellation before a response reached the transport: no served evidence.
            facts.duration = Some(self.started.elapsed());
            (self.observer)(facts);
        }
    }
}
impl RequestObservation {
    fn new(
        operation: GraphOperation,
        invocation: GraphInvocation,
        observer: GraphObserver,
    ) -> Self {
        Self(Arc::new(RequestState {
            facts: Mutex::new(Some(GraphObservation::new(operation, invocation))),
            started: Instant::now(),
            observer,
        }))
    }
    pub(super) fn from_context(context: &RequestContext<RoleServer>) -> Option<Self> {
        context.extensions.get::<Self>().cloned().or_else(|| {
            context
                .extensions
                .get::<axum::http::request::Parts>()?
                .extensions
                .get::<Self>()
                .cloned()
        })
    }
    pub(super) fn update(&self, update: impl FnOnce(&mut GraphObservation)) {
        if let Ok(mut guard) = self.0.facts.lock()
            && let Some(facts) = guard.as_mut()
        {
            update(facts);
        }
    }
    pub(super) fn snapshot(&self) -> Option<GraphObservation> {
        self.0.facts.lock().ok().and_then(|guard| *guard)
    }
    fn finish(
        &self,
        boundary: GraphOutputBoundary,
        served: Option<bool>,
        failure: Option<GraphFailureKind>,
    ) {
        let facts = self.0.facts.lock().ok().and_then(|mut guard| guard.take());
        if let Some(mut value) = facts {
            value.output_boundary = boundary;
            value.output_served = served;
            value.output_failure = failure;
            value.duration = Some(self.0.started.elapsed());
            (self.0.observer)(value);
        }
    }
}

pub(super) fn tool_operation(name: &str) -> GraphOperation {
    use GraphOperation as O;
    match name {
        "query_graph" => O::QueryGraph,
        "get_node" => O::GetNode,
        "get_neighbors" => O::GetNeighbors,
        "shortest_path" => O::ShortestPath,
        "query" => O::Search,
        "show" => O::Show,
        "callers" => O::Callers,
        "callees" => O::Callees,
        "impact" => O::Impact,
        "path" => O::Path,
        "stats" => O::Stats,
        "graph_stats" => O::GraphStats,
        "god_nodes" => O::GodNodes,
        "get_community" => O::GetCommunity,
        "list_prs" => O::ListPrs,
        "get_pr_impact" => O::GetPrImpact,
        "triage_prs" => O::TriagePrs,
        _ => O::Protocol,
    }
}
pub(super) fn resource_operation(kind: &str) -> GraphOperation {
    use GraphOperation as O;
    match kind {
        "stats" => O::ResourceStats,
        "graph" => O::ResourceGraph,
        "report" => O::ResourceReport,
        "god-nodes" => O::ResourceHubs,
        "communities" => O::ResourceCommunities,
        "computed-communities" => O::ResourceComputedCommunities,
        "surprises" => O::ResourceSurprises,
        "audit" => O::ResourceAudit,
        "questions" => O::ResourceQuestions,
        _ => O::Protocol,
    }
}
fn request_operation(request: &ClientRequest) -> GraphOperation {
    match request {
        ClientRequest::CallToolRequest(r) => tool_operation(&r.params.name),
        ClientRequest::InitializeRequest(_) => GraphOperation::Initialize,
        ClientRequest::ListToolsRequest(_) => GraphOperation::ToolsList,
        ClientRequest::ListResourcesRequest(_) => GraphOperation::ResourcesList,
        ClientRequest::PingRequest(_) => GraphOperation::Ping,
        _ => GraphOperation::Protocol,
    }
}

/// IDs exist only in this bounded in-process SDK correlation map, never in facts.
#[derive(Clone, Default)]
pub(super) struct StdioObservations(Arc<Mutex<HashMap<RequestId, RequestObservation>>>);
impl StdioObservations {
    pub(super) fn input(
        &self,
        message: &mut ClientJsonRpcMessage,
        observer: &Option<GraphObserver>,
    ) {
        if let JsonRpcMessage::Notification(notification) = message
            && let ClientNotification::CancelledNotification(cancelled) = &notification.notification
        {
            // rmcp suppresses the late response, so it cannot retire this slot.
            let observation = cancelled
                .params
                .request_id
                .as_ref()
                .and_then(|id| self.0.lock().ok()?.remove(id));
            if let Some(observation) = observation {
                // Finalize outside the correlation lock; late worker updates are ignored.
                observation.finish(GraphOutputBoundary::Unobserved, None, None);
            }
            return;
        }
        let (JsonRpcMessage::Request(request), Some(observer)) = (message, observer) else {
            return;
        };
        if let Ok(mut pending) = self.0.lock() {
            if pending.len() >= 64 || pending.contains_key(&request.id) {
                return;
            }
            let observation = RequestObservation::new(
                request_operation(&request.request),
                GraphInvocation::NativeStdio,
                observer.clone(),
            );
            request.request.extensions_mut().insert(observation.clone());
            pending.insert(request.id.clone(), observation);
        }
    }
    fn response(&self, message: &ServerJsonRpcMessage) -> Option<RequestObservation> {
        let (id, failed) = match message {
            JsonRpcMessage::Response(r) => (&r.id, false),
            JsonRpcMessage::Error(e) => (e.id.as_ref()?, true),
            _ => return None,
        };
        let observation = self.0.lock().ok()?.remove(id)?;
        observation.update(|f| {
            if f.execution_succeeded.is_none() {
                f.execution_succeeded = Some(!failed);
            }
            if failed && f.failure.is_none() {
                f.phase = GraphPhase::Protocol;
                f.fail(GraphFailureKind::Protocol);
            }
        });
        Some(observation)
    }
    fn abandon(&self) {
        let pending = self
            .0
            .lock()
            .ok()
            .map(|mut values| std::mem::take(&mut *values));
        if let Some(pending) = pending {
            for observation in pending.into_values() {
                observation.finish(GraphOutputBoundary::StdioFlush, Some(false), None);
            }
        }
    }
}

/// Delegates framing and I/O to rmcp/tokio-util; completion is a successful sink flush.
pub(super) struct ObservedSink<S> {
    sink: S,
    requests: StdioObservations,
    pending: Vec<RequestObservation>,
}
impl<S> ObservedSink<S> {
    pub(super) fn new(sink: S, requests: StdioObservations) -> Self {
        Self {
            sink,
            requests,
            pending: Vec::new(),
        }
    }
    fn complete(&mut self, served: bool) {
        for observation in self.pending.drain(..) {
            observation.finish(
                GraphOutputBoundary::StdioFlush,
                Some(served),
                (!served).then_some(GraphFailureKind::Io),
            );
        }
    }
}
impl<S> Sink<ServerJsonRpcMessage> for ObservedSink<S>
where
    S: Sink<ServerJsonRpcMessage> + Unpin,
{
    type Error = S::Error;
    fn poll_ready(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.sink).poll_ready(cx);
        if matches!(&result, Poll::Ready(Err(_))) {
            this.complete(false);
        }
        result
    }
    fn start_send(self: Pin<&mut Self>, message: ServerJsonRpcMessage) -> Result<(), Self::Error> {
        let this = self.get_mut();
        let observation = this.requests.response(&message);
        let result = Pin::new(&mut this.sink).start_send(message);
        if let Some(observation) = observation {
            if result.is_err() {
                observation.finish(
                    GraphOutputBoundary::StdioFlush,
                    Some(false),
                    Some(GraphFailureKind::Io),
                );
            } else if this.pending.len() < 64 {
                this.pending.push(observation);
            } else {
                observation.finish(GraphOutputBoundary::Unobserved, None, None);
            }
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.sink).poll_flush(cx);
        if let Poll::Ready(result) = &result {
            this.complete(result.is_ok());
        }
        result
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.sink).poll_close(cx);
        if let Poll::Ready(result) = &result {
            this.complete(result.is_ok());
        }
        result
    }
}
impl<S> Drop for ObservedSink<S> {
    fn drop(&mut self) {
        self.complete(false);
        self.requests.abandon();
    }
}

// The stateless SDK service uses Full JSON or SSE data bodies (no trailers).
struct ObservedBody<S> {
    stream: S,
    observation: RequestObservation,
    done: bool,
}
impl<S, T, E> Stream for ObservedBody<S>
where
    S: Stream<Item = Result<T, E>> + Unpin,
{
    type Item = Result<T, E>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_next(cx);
        match &result {
            Poll::Ready(None) => {
                this.done = true;
                this.observation
                    .finish(GraphOutputBoundary::HttpBody, Some(true), None);
            }
            Poll::Ready(Some(Err(_))) => {
                this.done = true;
                this.observation.finish(
                    GraphOutputBoundary::HttpBody,
                    Some(false),
                    Some(GraphFailureKind::Io),
                );
            }
            _ => {}
        }
        result
    }
}
impl<S> Drop for ObservedBody<S> {
    fn drop(&mut self) {
        if !self.done {
            self.observation
                .finish(GraphOutputBoundary::HttpBody, Some(false), None);
        }
    }
}
pub(super) async fn http(
    observer: Option<GraphObserver>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(observer) = observer else {
        return next.run(request).await;
    };
    let observation = RequestObservation::new(
        GraphOperation::Protocol,
        GraphInvocation::NativeHttp,
        observer,
    );
    request.extensions_mut().insert(observation.clone());
    let response = next.run(request).await;
    observation.update(|f| {
        if response.status().is_client_error() || response.status().is_server_error() {
            f.phase = GraphPhase::Protocol;
            f.fail(if response.status() == StatusCode::UNAUTHORIZED {
                GraphFailureKind::Authentication
            } else {
                GraphFailureKind::Protocol
            });
        }
    });
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        axum::body::Body::from_stream(ObservedBody {
            stream: body.into_data_stream(),
            observation,
            done: false,
        }),
    )
}

pub(super) fn protocol_failure(observer: &Option<GraphObserver>) {
    if let Some(observer) = observer {
        let observation = RequestObservation::new(
            GraphOperation::Protocol,
            GraphInvocation::NativeStdio,
            observer.clone(),
        );
        observation.update(|f| {
            f.phase = GraphPhase::Protocol;
            f.fail(GraphFailureKind::Protocol);
        });
        observation.finish(GraphOutputBoundary::Unobserved, None, None);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cancellation_tests;

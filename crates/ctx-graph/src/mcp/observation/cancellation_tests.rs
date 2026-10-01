use super::*;
use futures_util::SinkExt;
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CancelledNotificationParam},
    service::NotificationContext,
};
use tokio::sync::mpsc;

struct BlockedRequest {
    id: RequestId,
    release: std::sync::mpsc::Sender<()>,
}
struct FinishedRequest {
    id: RequestId,
    sdk_cancelled: bool,
    facts: Option<GraphObservation>,
}
#[derive(Clone)]
struct CancellationServer {
    graph: Graf,
    started: mpsc::UnboundedSender<BlockedRequest>,
    completed: mpsc::UnboundedSender<FinishedRequest>,
    cancelled: mpsc::UnboundedSender<RequestId>,
}
impl ServerHandler for CancellationServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
    async fn call_tool(
        &self,
        _: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let mut graph = self.graph.clone();
        graph.observation = RequestObservation::from_context(&context);
        let started = self.started.clone();
        let id = context.id.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let result = graph
            .run(None, move |_, facts| {
                started
                    .send(BlockedRequest { id, release })
                    .map_err(|_| anyhow::anyhow!("test driver dropped"))?;
                wait.recv().context("test release dropped")?;
                facts.result_count = Some(1);
                Ok(json!({"complete":true}))
            })
            .await;
        let _ = self.completed.send(FinishedRequest {
            id: context.id,
            sdk_cancelled: context.ct.is_cancelled(),
            facts: graph
                .observation
                .as_ref()
                .and_then(RequestObservation::snapshot),
        });
        Ok(graph.tool_result(result).into())
    }
    async fn on_cancelled(
        &self,
        params: CancelledNotificationParam,
        _: NotificationContext<RoleServer>,
    ) {
        if let Some(id) = params.request_id {
            let _ = self.cancelled.send(id);
        }
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(5), future)
        .await
        .expect("in-memory SDK progress")
}
fn client_message(value: Value) -> ClientJsonRpcMessage {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn sdk_stdio_cancellation_retires_slots_before_late_workers_and_keeps_normal_flush() {
    // Use the production codec/Sink/observation composition over a real duplex byte stream.
    let requests = StdioObservations::default();
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let values = Arc::new(Mutex::new(Vec::new()));
    let captured = values.clone();
    let pending = Arc::downgrade(&requests.0);
    let observer: GraphObserver = Arc::new(move |facts| {
        if let Some(pending) = pending.upgrade() {
            assert!(
                pending.try_lock().is_ok(),
                "callback holds correlation lock"
            );
        }
        captured.lock().unwrap().push(facts);
        let _ = event_tx.send(facts);
    });
    let (started_tx, mut started) = mpsc::unbounded_channel();
    let (completed_tx, mut completed) = mpsc::unbounded_channel();
    let (cancelled_tx, mut cancelled) = mpsc::unbounded_channel();
    let project = Arc::new(Project {
        db: PathBuf::new(),
        memory_dir: None,
        snapshot_max_bytes: DEFAULT_SNAPSHOT_BYTES,
        cache: Mutex::new(None),
    });
    let server = CancellationServer {
        graph: Graf {
            projects: Arc::new(BTreeMap::from([("default".into(), project)])),
            github_repo: None,
            workers: Arc::new(Semaphore::new(4)),
            tool_router: Graf::tool_router(),
            observation: None,
        },
        started: started_tx,
        completed: completed_tx,
        cancelled: cancelled_tx,
    };
    let (client_io, server_io) = tokio::io::duplex(MAX_MESSAGE);
    let (server_read, server_write) = tokio::io::split(server_io);
    let input_requests = requests.clone();
    let input = FramedRead::new(
        server_read,
        JsonRpcMessageCodec::<ClientJsonRpcMessage>::default(),
    )
    .map(move |message| {
        let mut message = message.unwrap();
        input_requests.input(&mut message, &Some(observer.clone()));
        message
    });
    let output = ObservedSink::new(
        FramedWrite::new(
            server_write,
            JsonRpcMessageCodec::<ServerJsonRpcMessage>::default(),
        ),
        requests.clone(),
    );
    let serving = tokio::spawn(async move {
        server
            .serve((output, input))
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap()
    });
    let (client_read, client_write) = tokio::io::split(client_io);
    let mut input = FramedRead::new(
        client_read,
        JsonRpcMessageCodec::<ServerJsonRpcMessage>::default(),
    );
    let mut output = FramedWrite::new(
        client_write,
        JsonRpcMessageCodec::<ClientJsonRpcMessage>::default(),
    );
    output.send(client_message(json!({"jsonrpc":"2.0", "id":0, "method":"initialize", "params":{
        "protocolVersion":"2025-03-26", "capabilities":{}, "clientInfo":{"name":"synthetic","version":"1"}
    }}))).await.unwrap();
    assert!(
        matches!(bounded(input.next()).await.unwrap().unwrap(), JsonRpcMessage::Response(response) if response.id == RequestId::Number(0))
    );
    assert_eq!(
        bounded(events.recv()).await.unwrap().output_served,
        Some(true)
    );
    output
        .send(client_message(
            json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
        ))
        .await
        .unwrap();

    // A healthy admitted worker establishes the paired ordinary completion contract.
    output.send(client_message(json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"query", "arguments":{}}}))).await.unwrap();
    let worker = bounded(started.recv()).await.unwrap();
    assert_eq!(worker.id, RequestId::Number(1));
    assert!(events.try_recv().is_err());
    worker.release.send(()).unwrap();
    let finished = bounded(completed.recv()).await.unwrap();
    assert_eq!(finished.id, RequestId::Number(1));
    assert!(!finished.sdk_cancelled);
    assert_eq!(finished.facts.unwrap().execution_succeeded, Some(true));
    assert!(
        matches!(bounded(input.next()).await.unwrap().unwrap(), JsonRpcMessage::Response(response) if response.id == RequestId::Number(1))
    );
    let ordinary = bounded(events.recv()).await.unwrap();
    assert_eq!(ordinary.execution_succeeded, Some(true));
    assert_eq!(ordinary.output_boundary, GraphOutputBoundary::StdioFlush);
    assert_eq!(ordinary.output_served, Some(true));

    for id in 2..66 {
        output.send(client_message(json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":"query", "arguments":{}}}))).await.unwrap();
        let worker = bounded(started.recv()).await.unwrap();
        assert_eq!(worker.id, RequestId::Number(id));
        assert_eq!(requests.0.lock().unwrap().len(), 1);
        assert!(events.try_recv().is_err());
        output.send(client_message(json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":id, "reason":"private-cancellation-sentinel"}}))).await.unwrap();
        let cancellation = bounded(events.recv()).await.unwrap();
        assert_eq!(cancellation.operation, GraphOperation::Search);
        assert_eq!(cancellation.execution_succeeded, None);
        assert_eq!(
            cancellation.output_boundary,
            GraphOutputBoundary::Unobserved
        );
        assert_eq!(cancellation.output_served, None);
        assert_eq!(cancellation.output_failure, None);
        assert_eq!(cancellation.failure, None);
        assert!(cancellation.duration.is_some());
        assert!(!format!("{cancellation:?}").contains("sentinel"));
        assert!(requests.0.lock().unwrap().is_empty());
        // The SDK processed cancellation before the admitted Graph worker is released.
        assert_eq!(
            bounded(cancelled.recv()).await.unwrap(),
            RequestId::Number(id)
        );
        assert!(completed.try_recv().is_err());
        worker.release.send(()).unwrap();
        let finished = bounded(completed.recv()).await.unwrap();
        assert_eq!(finished.id, RequestId::Number(id));
        assert!(finished.sdk_cancelled);
        assert_eq!(
            finished.facts, None,
            "late completion reopened finalized facts"
        );
        assert!(events.try_recv().is_err(), "late completion emitted twice");
    }

    // All 64 cancellations must leave observation capacity for a normal response flush.
    output
        .send(client_message(
            json!({"jsonrpc":"2.0", "id":66, "method":"ping"}),
        ))
        .await
        .unwrap();
    assert!(
        matches!(bounded(input.next()).await.unwrap().unwrap(), JsonRpcMessage::Response(response) if response.id == RequestId::Number(66)),
        "SDK sent a cancelled response"
    );
    let normal = bounded(events.recv()).await.unwrap();
    assert_eq!(normal.operation, GraphOperation::Ping);
    assert_eq!(normal.execution_succeeded, Some(true));
    assert_eq!(normal.output_boundary, GraphOutputBoundary::StdioFlush);
    assert_eq!(normal.output_served, Some(true));
    assert!(requests.0.lock().unwrap().is_empty());
    let frozen = values.lock().unwrap().clone();
    assert_eq!(frozen.len(), 67); // initialize, paired ordinary worker, 64 cancellations, ping
    assert!(events.try_recv().is_err());
    output.close().await.unwrap();
    bounded(serving).await.unwrap();
    assert!(
        bounded(input.next()).await.is_none(),
        "cancelled response survived SDK shutdown drain"
    );
    assert_eq!(
        *values.lock().unwrap(),
        frozen,
        "shutdown changed cancellation facts or latency"
    );
}

#[test]
fn cancellation_preserves_already_observed_execution_and_ignores_unknown_ids() {
    let requests = StdioObservations::default();
    let values = Arc::new(Mutex::new(Vec::new()));
    let captured = values.clone();
    let observer: GraphObserver = Arc::new(move |facts| captured.lock().unwrap().push(facts));
    let mut request = client_message(json!({"jsonrpc":"2.0", "id":1, "method":"ping"}));
    requests.input(&mut request, &Some(observer));
    let JsonRpcMessage::Request(request) = request else {
        panic!("request")
    };
    let observation = request
        .request
        .extensions()
        .get::<RequestObservation>()
        .unwrap();
    observation.update(|facts| facts.execution_succeeded = Some(true));
    for params in [
        json!({}),
        json!({"requestId":2}),
        json!({"requestId":1}),
        json!({"requestId":1}),
    ] {
        // Retirement is independent of the callback option passed to later input.
        requests.input(
            &mut client_message(
                json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":params}),
            ),
            &None,
        );
    }
    let values = values.lock().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].execution_succeeded, Some(true));
    assert_eq!(values[0].output_boundary, GraphOutputBoundary::Unobserved);
    assert_eq!(values[0].output_served, None);
    assert_eq!(observation.snapshot(), None);
    assert!(requests.0.lock().unwrap().is_empty());
}

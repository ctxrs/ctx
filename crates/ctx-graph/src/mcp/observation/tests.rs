use super::*;
use futures_util::{SinkExt, stream};
use std::io;

fn observed() -> (RequestObservation, Arc<Mutex<Vec<GraphObservation>>>) {
    let values = Arc::new(Mutex::new(Vec::new()));
    let captured = values.clone();
    let observer: GraphObserver = Arc::new(move |facts| captured.lock().unwrap().push(facts));
    (
        RequestObservation::new(
            GraphOperation::Search,
            GraphInvocation::NativeHttp,
            observer,
        ),
        values,
    )
}

#[tokio::test]
async fn body_handoff_requires_eof_and_emits_once() {
    let (observation, values) = observed();
    observation.update(|f| f.execution_succeeded = Some(true));
    let mut body = ObservedBody {
        stream: stream::iter([Ok::<_, io::Error>(b"payload")]),
        observation,
        done: false,
    };
    assert!(body.next().await.unwrap().is_ok());
    assert!(values.lock().unwrap().is_empty());
    assert!(body.next().await.is_none());
    drop(body);
    let values = values.lock().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].output_served, Some(true));
    assert_eq!(values[0].output_boundary, GraphOutputBoundary::HttpBody);
}

#[tokio::test]
async fn failed_or_abandoned_body_never_claims_handoff() {
    let (observation, values) = observed();
    observation.update(|f| f.execution_succeeded = Some(true));
    let mut body = ObservedBody {
        stream: stream::iter([Err::<&[u8], _>(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "private-body-sentinel",
        ))]),
        observation,
        done: false,
    };
    assert!(body.next().await.unwrap().is_err());
    drop(body);
    assert_eq!(values.lock().unwrap().len(), 1);
    assert_eq!(values.lock().unwrap()[0].output_served, Some(false));
    assert!(!format!("{:?}", values.lock().unwrap()).contains("sentinel"));
    let (observation, values) = observed();
    let body = ObservedBody {
        stream: stream::pending::<Result<&[u8], io::Error>>(),
        observation,
        done: false,
    };
    drop(body);
    assert_eq!(values.lock().unwrap()[0].output_served, Some(false));
    assert_eq!(values.lock().unwrap()[0].execution_succeeded, None);
}

struct TestSink {
    fail_flush: bool,
}
impl Sink<ServerJsonRpcMessage> for TestSink {
    type Error = io::Error;
    fn poll_ready(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, _: ServerJsonRpcMessage) -> io::Result<()> {
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(if self.fail_flush {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "private-sink-sentinel",
            ))
        } else {
            Ok(())
        })
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[tokio::test]
async fn stdio_waits_for_flush_and_keeps_ids_out_of_facts() {
    for fail_flush in [false, true] {
        let (_, values) = observed();
        values.lock().unwrap().clear();
        let captured = values.clone();
        let observer: GraphObserver = Arc::new(move |facts| captured.lock().unwrap().push(facts));
        let requests = StdioObservations::default();
        let mut request: ClientJsonRpcMessage = serde_json::from_value(json!({"jsonrpc":"2.0","id":"private-id-sentinel","method":"tools/call","params":{"name":"query","arguments":{"text":"private-query-sentinel"}}})).unwrap();
        requests.input(&mut request, &Some(observer));
        let JsonRpcMessage::Request(request) = request else {
            panic!("request")
        };
        let response = JsonRpcMessage::Error(rmcp::model::JsonRpcError::new(
            Some(request.id),
            ErrorData::invalid_params("private-error-sentinel", None),
        ));
        let mut sink = ObservedSink::new(TestSink { fail_flush }, requests);
        sink.feed(response).await.unwrap();
        assert!(values.lock().unwrap().is_empty());
        assert_eq!(sink.flush().await.is_err(), fail_flush);
        drop(sink);
        let values = values.lock().unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].operation, GraphOperation::Search);
        assert_eq!(values[0].execution_succeeded, Some(false));
        assert_eq!(values[0].output_served, Some(!fail_flush));
        assert_eq!(values[0].output_boundary, GraphOutputBoundary::StdioFlush);
        assert!(!format!("{values:?}").contains("sentinel"));
    }
}

#[test]
fn all_native_tools_have_closed_operations() {
    for tool in Graf::tool_router().list_all() {
        assert_ne!(
            tool_operation(&tool.name),
            GraphOperation::Protocol,
            "{}",
            tool.name
        );
    }
    for (resource, _) in resources::RESOURCES {
        assert_ne!(resource_operation(resource), GraphOperation::Protocol);
    }
    assert_eq!(
        tool_operation("unknown-private-name"),
        GraphOperation::Protocol
    );
    assert_eq!(
        resource_operation("unknown-private-uri"),
        GraphOperation::Protocol
    );
}

#[tokio::test]
async fn worker_admission_and_cache_facts_come_from_actual_work() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("graph.db");
    Store::create(&db)?.import_graph(ctx_graph_core::model::ImportedGraph {
        nodes: vec![],
        edges: vec![],
        metadata: json!({}),
    })?;
    let (observation, _) = observed();
    let project = Arc::new(Project {
        db,
        memory_dir: None,
        snapshot_max_bytes: DEFAULT_SNAPSHOT_BYTES,
        cache: Mutex::new(None),
    });
    let mut server = Graf {
        projects: Arc::new(BTreeMap::from([("default".into(), project)])),
        github_repo: None,
        workers: Arc::new(Semaphore::new(4)),
        tool_router: Graf::tool_router(),
        observation: Some(observation.clone()),
    };
    server
        .run(None, |project, facts| {
            let snapshot = project.snapshot(facts)?;
            snapshot.analysis(facts)?;
            Ok(())
        })
        .await?;
    let first = observation.snapshot().unwrap();
    assert_eq!(first.snapshot_cache_hit, Some(false));
    assert_eq!(first.analysis_cache_hit, Some(false));
    assert_eq!(first.execution_succeeded, Some(true));
    let (second, _) = observed();
    server.observation = Some(second.clone());
    server
        .run(None, |project, facts| {
            let snapshot = project.snapshot(facts)?;
            snapshot.analysis(facts)?;
            Ok(())
        })
        .await?;
    assert_eq!(second.snapshot().unwrap().snapshot_cache_hit, Some(true));
    assert_eq!(second.snapshot().unwrap().analysis_cache_hit, Some(true));
    let (missing, _) = observed();
    server.observation = Some(missing.clone());
    assert!(
        server
            .run(
                Some("private-project-sentinel".into()),
                |_, _| -> Result<()> { panic!("must not enter worker") }
            )
            .await
            .is_err()
    );
    assert_eq!(
        missing.snapshot().unwrap().failure.unwrap().kind,
        GraphFailureKind::UnknownProject
    );
    assert!(!format!("{:?}", missing.snapshot()).contains("sentinel"));
    let (capacity, _) = observed();
    server.observation = Some(capacity.clone());
    server.workers = Arc::new(Semaphore::new(0));
    assert!(
        server
            .run(None, |_, _| -> Result<()> {
                panic!("must not enter worker")
            })
            .await
            .is_err()
    );
    assert_eq!(
        capacity.snapshot().unwrap().failure.unwrap().kind,
        GraphFailureKind::Capacity
    );
    Ok(())
}

#[tokio::test]
async fn startup_registration_failure_is_observed_before_any_transport_starts() {
    let temp = tempfile::tempdir().unwrap();
    let mut facts = GraphObservation::new(GraphOperation::Serve, GraphInvocation::Cli);
    let args = ServeArgs {
        transport: Transport::Stdio,
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        path: "/mcp".into(),
        bearer_token_env: None,
        allowed_host: vec![],
        project: vec![],
        github_repo: None,
        memory_dir: None,
        snapshot_max_bytes: DEFAULT_SNAPSHOT_BYTES,
    };
    assert!(
        transport::serve_observed(temp.path().join("missing.db"), args, &mut facts, None)
            .await
            .is_err()
    );
    assert_eq!(facts.lifecycle, Some(GraphLifecycle::StartFailed));
    assert_eq!(facts.failure.unwrap().phase, GraphPhase::Registration);
    assert_eq!(facts.execution_succeeded, Some(false));
    assert_eq!(facts.output_served, None);
}

#[test]
fn correlation_capacity_skips_optional_facts_without_rejecting_requests() {
    let (_, values) = observed();
    values.lock().unwrap().clear();
    let captured = values.clone();
    let observer: GraphObserver = Arc::new(move |facts| captured.lock().unwrap().push(facts));
    let requests = StdioObservations::default();
    for id in 0..65 {
        let mut request: ClientJsonRpcMessage =
            serde_json::from_value(json!({"jsonrpc":"2.0", "id":id, "method":"ping"})).unwrap();
        requests.input(&mut request, &Some(observer.clone()));
        let JsonRpcMessage::Request(request) = request else {
            panic!("request")
        };
        assert_eq!(
            request
                .request
                .extensions()
                .get::<RequestObservation>()
                .is_some(),
            id < 64
        );
    }
    assert_eq!(requests.0.lock().unwrap().len(), 64);
    requests.abandon();
    assert_eq!(values.lock().unwrap().len(), 64);
    assert!(
        values
            .lock()
            .unwrap()
            .iter()
            .all(|value| value.output_served == Some(false))
    );
}

#[test]
fn response_size_boundary_reports_failure_only_after_the_allowed_payload() {
    let (observation, _) = observed();
    let mut server = Graf {
        projects: Arc::new(BTreeMap::new()),
        github_repo: None,
        workers: Arc::new(Semaphore::new(4)),
        tool_router: Graf::tool_router(),
        observation: Some(observation.clone()),
    };
    let maximum = Value::String("x".repeat(MAX_MESSAGE / 2 - 2));
    assert_eq!(serde_json::to_vec(&maximum).unwrap().len(), MAX_MESSAGE / 2);
    assert_ne!(server.tool_result(Ok(maximum)).is_error, Some(true));
    assert_eq!(observation.snapshot().unwrap().failure, None);
    let (failed, _) = observed();
    server.observation = Some(failed.clone());
    let oversized = Value::String("x".repeat(MAX_MESSAGE / 2 - 1));
    assert_eq!(server.tool_result(Ok(oversized)).is_error, Some(true));
    assert_eq!(
        failed.snapshot().unwrap().failure.unwrap().kind,
        GraphFailureKind::ResponseLimit
    );
    assert_eq!(failed.snapshot().unwrap().execution_succeeded, Some(false));
    assert_eq!(failed.snapshot().unwrap().output_served, None);
}

use super::*;
use std::sync::Mutex;

fn observer() -> (ServerObserver, Arc<Mutex<Vec<ServerObservation>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    (
        Arc::new(move |event| captured.lock().unwrap().push(event)),
        events,
    )
}

#[tokio::test]
async fn head_suppression_is_not_a_transport_failure_or_delivered_payload() {
    let root = tempfile::tempdir().unwrap();
    let (observer, events) = observer();
    let server = Arc::new(
        HistoryServer::open_with_observer(ServerConfig::new(root.path()), Some(observer)).unwrap(),
    );
    let response = router(server)
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(to_bytes(response.into_body(), 100)
        .await
        .unwrap()
        .is_empty());
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [ServerObservation::Request {
            operation: ServerOperation::Health,
            failure: None,
            body_outcome: ServerBodyOutcome::Suppressed,
            response_bytes: 0,
            body_handed_off: None,
            ..
        }]
    ));
}

#[tokio::test]
async fn actual_search_handler_attaches_local_read_and_execution_to_transport_terminal() {
    let root = tempfile::tempdir().unwrap();
    let (mut server, token) = bootstrap(root.path());
    let (observe, events) = observer();
    server.observer = Some(observe);
    let response = router(Arc::new(server))
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/collections/{}/search?q=synthetic&limit=5",
                    token.collection
                ))
                .header(
                    "authorization",
                    format!("Bearer {}", token.credential.secret),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ServerObservation::Execution { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ServerObservation::Read { .. }))
            .count(),
        1
    );
    let requests: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, ServerObservation::Request { .. }))
        .collect();
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0], ServerObservation::Request {
        body_handed_off: Some(true), response_bytes,
        execution: Some(ServerExecutionFacts { failure: None, .. }),
        read: Some(ServerReadFacts { returned: 0, complete: Some(true), .. }), ..
    } if *response_bytes == bytes.len() as u64));
}

#[tokio::test]
async fn registered_operation_survives_extraction_auth_and_unknown_route_denials() {
    let root = tempfile::tempdir().unwrap();
    let (observer, events) = observer();
    let mut config = ServerConfig::new(root.path());
    config.max_chunk_bytes = 1024;
    let server = Arc::new(HistoryServer::open_with_observer(config, Some(observer)).unwrap());
    for (method, uri, body, expected, operation, failure) in [
        (
            "GET",
            "/v1/collections/private-label/search?q=private-query&limit=1",
            String::new(),
            StatusCode::UNAUTHORIZED,
            ServerOperation::Search,
            ServerFailure::Unauthorized,
        ),
        (
            "GET",
            "/v1/collections/private-label/search?q=private-query&limit=bad",
            String::new(),
            StatusCode::BAD_REQUEST,
            ServerOperation::Search,
            ServerFailure::Invalid,
        ),
        (
            "POST",
            "/v1/enroll",
            "x".repeat(256 * 1024 + 1),
            StatusCode::PAYLOAD_TOO_LARGE,
            ServerOperation::Enroll,
            ServerFailure::BodyTooLarge,
        ),
        (
            "GET",
            "/private-path",
            String::new(),
            StatusCode::NOT_FOUND,
            ServerOperation::Unknown,
            ServerFailure::NotFound,
        ),
        (
            "POST",
            "/healthz",
            String::new(),
            StatusCode::METHOD_NOT_ALLOWED,
            ServerOperation::Unknown,
            ServerFailure::Method,
        ),
    ] {
        let response = router(server.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let _ = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let mut events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(events[0], ServerObservation::Request { operation: actual, failure: Some(actual_failure), response_class: ServerResponseClass::ClientError, body_handed_off: Some(true), execution: None, read: None, .. } if actual == operation && actual_failure == failure)
        );
        let safe = format!("{:?}", events[0]);
        for marker in ["private-label", "private-query", "private-path"] {
            assert!(!safe.contains(marker));
        }
        events.clear();
    }
}

#[test]
fn acceptance_replay_index_and_read_facts_follow_real_commits() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(&root.path().join("source"), "alpha", &["telemetry apricot"]);
    let (mut server, token) = bootstrap(root.path());
    let (observer, events) = observer();
    server.observer = Some(observer);
    let request = stage(
        &server,
        &token.credential.secret,
        &token.collection,
        &input,
        "synthetic-publication",
        None,
        "synthetic-key",
    );
    let original = server
        .publish(&token.credential.secret, &token.collection, request.clone())
        .unwrap();
    let replay = server
        .publish(&token.credential.secret, &token.collection, request)
        .unwrap();
    assert_eq!(original.sequence, replay.sequence);
    server.index_pending(&token.collection, 16).unwrap();
    let (response, read) = server
        .search_with_facts(
            &token.credential.secret,
            &token.collection,
            SearchRequest {
                q: "apricot".into(),
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(read.returned, 1);
    assert_eq!(
        read.bytes,
        Some(serde_json::to_vec(&response).unwrap().len() as u64)
    );
    let (page, facts) = server
        .read_session_with_facts(
            &token.credential.secret,
            &token.collection,
            &response.results[0].session_citation,
            SessionRequest {
                limit: 1,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(facts.returned, page.events.len() as u64);
    assert_eq!(facts.has_more, Some(false));
    let events = events.lock().unwrap();
    let accepted: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let ServerObservation::Publication(facts) = event {
                Some(*facts)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(accepted.len(), 2);
    assert!(!accepted[0].replay);
    assert!(accepted[1].replay);
    assert_eq!(accepted[0].records, Some(input.member.records));
    assert!(events.iter().any(|event| matches!(
        event,
        ServerObservation::Index {
            facts: ServerIndexFacts {
                activated: true,
                processed_operations: Some(1),
                coverage_lag: Some(0),
                ..
            },
            failure: None,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(event, ServerObservation::Read { operation: ServerOperation::Search, facts } if *facts == read)));
}

#[test]
fn invalid_startup_and_ready_callback_failure_emit_closed_failure_and_drain_hook() {
    let root = tempfile::tempdir().unwrap();
    let (observer, events) = observer();
    let mut config = ServerConfig::new(root.path().join("invalid"));
    config.max_in_flight = 0;
    assert!(HistoryServer::open_with_observer(config, Some(observer.clone())).is_err());
    assert!(!root.path().join("invalid").exists());
    assert!(matches!(
        events.lock().unwrap()[0],
        ServerObservation::Lifecycle {
            kind: ServerLifecycle::Failed,
            stage: ServerStage::Configuration,
            failure: Some(ServerFailure::Invalid),
            ..
        }
    ));
    events.lock().unwrap().clear();
    let mut config = ServerConfig::new(root.path().join("server"));
    config.bind.set_port(0);
    let server = Arc::new(HistoryServer::open_with_observer(config, Some(observer)).unwrap());
    let drained = Arc::new(Mutex::new(Vec::new()));
    let capture = drained.clone();
    assert!(serve_blocking_with_hooks(
        server,
        |_| Err(Error::Invalid("synthetic callback failure")),
        Some(Arc::new(move |tick| capture.lock().unwrap().push(tick)))
    )
    .is_err());
    assert_eq!(*drained.lock().unwrap(), [ServerRuntimeTick::Failed]);
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [ServerObservation::Lifecycle {
            kind: ServerLifecycle::Failed,
            stage: ServerStage::ReadyCallback,
            failure: Some(ServerFailure::Invalid),
            ..
        }]
    ));
}

#[test]
fn backlog_cap_and_busy_authority_do_not_become_unbounded_scans_or_false_zeroes() {
    let root = tempfile::tempdir().unwrap();
    let (observer, events) = observer();
    let server =
        HistoryServer::open_with_observer(ServerConfig::new(root.path()), Some(observer)).unwrap();
    let mut connection = server.authority.lock().unwrap();
    let tx = connection.transaction().unwrap();
    for n in 0..1002 {
        tx.execute(
            "INSERT INTO collections(id,name) VALUES (?1,'synthetic')",
            [format!("synthetic-{n}")],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    server.lifecycle(
        ServerLifecycle::Liveness,
        ServerStage::Serve,
        std::time::Duration::ZERO,
        None,
    );
    drop(connection);
    server.lifecycle(
        ServerLifecycle::Liveness,
        ServerStage::Serve,
        std::time::Duration::ZERO,
        None,
    );
    let events = events.lock().unwrap();
    assert!(matches!(
        events[0],
        ServerObservation::Lifecycle { backlog: None, .. }
    ));
    assert!(matches!(
        events[1],
        ServerObservation::Lifecycle {
            backlog: Some(ServerBacklog {
                collections_capped: 1001,
                pending_operations_capped: 0,
                staged_uploads_capped: 0
            }),
            ..
        }
    ));
}

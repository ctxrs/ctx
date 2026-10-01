use super::*;
use axum::body::{Body, HttpBody};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

/// The typed facts stay with this response, including while other requests
/// finish. No global "last result" or request identity is involved.
pub(super) struct ObservedJson<T> {
    pub result: Result<T>,
    pub execution: ServerExecutionFacts,
    pub read: Option<ServerReadFacts>,
}

impl<T: Serialize> IntoResponse for ObservedJson<T> {
    fn into_response(self) -> Response {
        let mut response = match self.result {
            Ok(value) => {
                let mut response = Json(value).into_response();
                if response.status().is_server_error() {
                    response.extensions_mut().insert(ServerFailure::Json);
                }
                response
            }
            Err(error) => {
                let mut response = error.into_response();
                if let Some(failure) = self.execution.failure {
                    response.extensions_mut().insert(failure);
                }
                response
            }
        };
        response.extensions_mut().insert(self.execution);
        if let Some(read) = self.read {
            response.extensions_mut().insert(read);
        }
        response
    }
}

#[derive(Clone, Copy)]
pub(super) struct HeadResponse;

pub(super) fn observe_response(
    server: &HistoryServer,
    operation: ServerOperation,
    started: Instant,
    response: Response,
    failure: Option<ServerFailure>,
) -> Response {
    let Some(observer) = &server.observer else {
        return response;
    };
    let response_class = match response.status().as_u16() {
        200..=299 => ServerResponseClass::Success,
        300..=399 => ServerResponseClass::Redirect,
        400..=499 => ServerResponseClass::ClientError,
        500..=599 => ServerResponseClass::ServerError,
        _ => ServerResponseClass::Other,
    };
    let is_head = response.extensions().get::<HeadResponse>().is_some();
    let execution = response.extensions().get::<ServerExecutionFacts>().copied();
    let read = response.extensions().get::<ServerReadFacts>().copied();
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(ObservedBody {
            body: Box::pin(body),
            observer: Some(observer.clone()),
            operation,
            started,
            failure,
            response_class,
            execution,
            read,
            bytes: 0,
            is_head,
        }),
    )
}

struct ObservedBody {
    body: Pin<Box<Body>>,
    observer: Option<ServerObserver>,
    operation: ServerOperation,
    started: Instant,
    failure: Option<ServerFailure>,
    response_class: ServerResponseClass,
    execution: Option<ServerExecutionFacts>,
    read: Option<ServerReadFacts>,
    bytes: u64,
    is_head: bool,
}

impl ObservedBody {
    fn finish(&mut self, outcome: ServerBodyOutcome) {
        if let Some(observer) = self.observer.take() {
            observer(ServerObservation::Request {
                operation: self.operation,
                duration: self.started.elapsed(),
                failure: self.failure.or(match outcome {
                    ServerBodyOutcome::Complete | ServerBodyOutcome::Suppressed => None,
                    ServerBodyOutcome::Failed => Some(ServerFailure::Body),
                    ServerBodyOutcome::Dropped => Some(ServerFailure::Interrupted),
                }),
                response_class: self.response_class,
                body_outcome: outcome,
                body_handed_off: (outcome != ServerBodyOutcome::Suppressed)
                    .then_some(outcome == ServerBodyOutcome::Complete),
                response_bytes: self.bytes,
                execution: self.execution,
                read: self.read,
            });
        }
    }
}

impl HttpBody for ObservedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = this.body.as_mut().poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.bytes = this.bytes.saturating_add(data.len() as u64);
                }
                if this.body.is_end_stream() {
                    this.finish(ServerBodyOutcome::Complete);
                }
            }
            Poll::Ready(Some(Err(_))) => this.finish(ServerBodyOutcome::Failed),
            Poll::Ready(None) => this.finish(ServerBodyOutcome::Complete),
            Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

impl Drop for ObservedBody {
    fn drop(&mut self) {
        // Hyper may skip polling an already-empty body. There are no remaining
        // frames to hand off in that case; this still says nothing about peers.
        self.finish(if self.is_head {
            ServerBodyOutcome::Suppressed
        } else if self.body.is_end_stream() {
            ServerBodyOutcome::Complete
        } else {
            ServerBodyOutcome::Dropped
        });
    }
}

/// Cancellation may drop the middleware future before there is a body to wrap.
pub(super) struct RequestGuard {
    observer: Option<ServerObserver>,
    operation: ServerOperation,
    started: Instant,
}

impl RequestGuard {
    pub fn new(server: &HistoryServer, operation: ServerOperation, started: Instant) -> Self {
        Self {
            observer: server.observer.clone(),
            operation,
            started,
        }
    }
    pub fn disarm(&mut self) {
        self.observer = None;
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Some(observer) = self.observer.take() {
            observer(ServerObservation::Request {
                operation: self.operation,
                duration: self.started.elapsed(),
                failure: Some(ServerFailure::Interrupted),
                body_handed_off: Some(false),
                response_class: ServerResponseClass::Unavailable,
                body_outcome: ServerBodyOutcome::Dropped,
                response_bytes: 0,
                execution: None,
                read: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn observed_server() -> (
        tempfile::TempDir,
        HistoryServer,
        Arc<Mutex<Vec<ServerObservation>>>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let facts = Arc::new(Mutex::new(Vec::new()));
        let capture = facts.clone();
        let server = HistoryServer::open_with_observer(
            ServerConfig::new(root.path()),
            Some(Arc::new(move |fact| capture.lock().unwrap().push(fact))),
        )
        .unwrap();
        (root, server, facts)
    }

    fn response(server: &HistoryServer, returned: u64) -> Response {
        let mut response = Body::from("abc").into_response();
        response.extensions_mut().insert(ServerReadFacts {
            returned,
            ..Default::default()
        });
        response.extensions_mut().insert(ServerExecutionFacts {
            duration: Duration::from_millis(returned),
            failure: None,
        });
        observe_response(
            server,
            ServerOperation::Search,
            Instant::now(),
            response,
            None,
        )
    }

    #[tokio::test]
    async fn completion_keeps_facts_with_each_body_when_responses_finish_out_of_order() {
        let (_root, server, facts) = observed_server();
        let first = response(&server, 3);
        let second = response(&server, 7);
        assert!(facts.lock().unwrap().is_empty());
        assert_eq!(
            &axum::body::to_bytes(second.into_body(), 100).await.unwrap()[..],
            b"abc"
        );
        assert_eq!(
            &axum::body::to_bytes(first.into_body(), 100).await.unwrap()[..],
            b"abc"
        );
        let facts = facts.lock().unwrap();
        assert_eq!(facts.len(), 2);
        for (fact, count) in facts.iter().zip([7, 3]) {
            assert!(matches!(fact, ServerObservation::Request {
                body_outcome: ServerBodyOutcome::Complete, body_handed_off: Some(true),
                response_class: ServerResponseClass::Success, response_bytes: 3,
                read: Some(read), execution: Some(execution), failure: None, ..
            } if read.returned == count && execution.duration == Duration::from_millis(count)));
        }
    }

    #[test]
    fn unpolled_nonempty_body_and_interrupted_middleware_are_distinct_from_delivery() {
        let (_root, server, facts) = observed_server();
        drop(response(&server, 9));
        drop(RequestGuard::new(
            &server,
            ServerOperation::Event,
            Instant::now(),
        ));
        let facts = facts.lock().unwrap();
        assert_eq!(facts.len(), 2);
        assert!(matches!(
            facts[0],
            ServerObservation::Request {
                body_outcome: ServerBodyOutcome::Dropped,
                body_handed_off: Some(false),
                response_class: ServerResponseClass::Success,
                response_bytes: 0,
                read: Some(ServerReadFacts { returned: 9, .. }),
                ..
            }
        ));
        assert!(matches!(
            facts[1],
            ServerObservation::Request {
                response_class: ServerResponseClass::Unavailable,
                execution: None,
                read: None,
                failure: Some(ServerFailure::Interrupted),
                ..
            }
        ));
    }

    struct BrokenBody(bool);
    impl HttpBody for BrokenBody {
        type Data = Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<http_body::Frame<Bytes>, Self::Error>>> {
            if std::mem::replace(&mut self.0, true) {
                Poll::Ready(Some(Err(std::io::Error::other("synthetic body failure"))))
            } else {
                Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(b"abc")))))
            }
        }
    }

    #[tokio::test]
    async fn body_error_reports_only_bytes_actually_yielded_and_no_success() {
        let (_root, server, facts) = observed_server();
        let response = observe_response(
            &server,
            ServerOperation::Event,
            Instant::now(),
            Body::new(BrokenBody(false)).into_response(),
            None,
        );
        assert!(axum::body::to_bytes(response.into_body(), 100)
            .await
            .is_err());
        let facts = facts.lock().unwrap();
        assert_eq!(facts.len(), 1);
        assert!(matches!(
            facts[0],
            ServerObservation::Request {
                body_outcome: ServerBodyOutcome::Failed,
                failure: Some(ServerFailure::Body),
                response_bytes: 3,
                body_handed_off: Some(false),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn absent_observer_preserves_response_bytes_without_wrapping() {
        let root = tempfile::tempdir().unwrap();
        let server = HistoryServer::open(ServerConfig::new(root.path())).unwrap();
        let response = response(&server, 2);
        assert_eq!(
            &axum::body::to_bytes(response.into_body(), 100)
                .await
                .unwrap()[..],
            b"abc"
        );
    }
}

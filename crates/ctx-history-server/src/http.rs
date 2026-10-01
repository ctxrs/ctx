use crate::*;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put, MethodRouter},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
mod response;
mod runtime;
use response::{observe_response, HeadResponse, ObservedJson, RequestGuard};
pub use runtime::{
    serve, serve_blocking, serve_blocking_with_hooks, serve_blocking_with_ready, serve_with_hooks,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct HttpState {
    server: Arc<HistoryServer>,
    requests: Arc<Semaphore>,
    work: Arc<Semaphore>,
    request_timeout: Duration,
}

pub fn router(server: Arc<HistoryServer>) -> Router {
    let body_limit = server.config.max_chunk_bytes.max(256 * 1024);
    let state = HttpState {
        requests: Arc::new(Semaphore::new(server.config.max_in_flight)),
        work: Arc::new(Semaphore::new(server.config.max_in_flight)),
        request_timeout: Duration::from_secs(30),
        server,
    };
    Router::new()
        .route(
            "/healthz",
            observed(
                get(|| async { Json(serde_json::json!({"alive":true})) }),
                &state,
                ServerOperation::Health,
            ),
        )
        .route(
            "/v1/enroll",
            observed(post(enroll), &state, ServerOperation::Enroll),
        )
        .route(
            "/v1/principals",
            observed(get(principals), &state, ServerOperation::Principals),
        )
        .route(
            "/v1/principals/{principal}/credentials",
            observed(get(credentials), &state, ServerOperation::Credentials),
        )
        .route(
            "/v1/principals/{principal}/revoke",
            observed(
                post(revoke_principal),
                &state,
                ServerOperation::RevokePrincipal,
            ),
        )
        .route(
            "/v1/credentials/{credential_id}/revoke",
            observed(
                post(revoke_credential),
                &state,
                ServerOperation::RevokeCredential,
            ),
        )
        .route(
            "/v1/collections/{collection}/whoami",
            observed(get(whoami), &state, ServerOperation::Whoami),
        )
        .route(
            "/v1/collections/{collection}/invite",
            observed(post(invite), &state, ServerOperation::Invite),
        )
        .route(
            "/v1/collections/{collection}/grants",
            observed(post(grants), &state, ServerOperation::Grants),
        )
        .route(
            "/v1/collections/{collection}/members/{principal}/revoke",
            observed(post(revoke), &state, ServerOperation::RevokeMember),
        )
        .route(
            "/v1/collections/{collection}/uploads",
            observed(post(begin_upload), &state, ServerOperation::BeginUpload),
        )
        .route(
            "/v1/collections/{collection}/uploads/{id}",
            observed(get(upload_status), &state, ServerOperation::UploadStatus).merge(observed(
                put(upload_chunk),
                &state,
                ServerOperation::UploadChunk,
            )),
        )
        .route(
            "/v1/collections/{collection}/revisions",
            observed(post(publish), &state, ServerOperation::Publish),
        )
        .route(
            "/v1/collections/{collection}/operations/cancel",
            observed(post(cancel_publish), &state, ServerOperation::CancelPublish),
        )
        .route(
            "/v1/collections/{collection}/withdraw",
            observed(post(withdraw), &state, ServerOperation::Withdraw),
        )
        .route(
            "/v1/collections/{collection}/remove",
            observed(post(remove), &state, ServerOperation::Remove),
        )
        .route(
            "/v1/collections/{collection}/receipts/{id}",
            observed(get(receipt), &state, ServerOperation::Receipt),
        )
        .route(
            "/v1/collections/{collection}/publications",
            observed(get(publications), &state, ServerOperation::Publications),
        )
        .route(
            "/v1/collections/{collection}/publications/{id}",
            observed(get(publication_state), &state, ServerOperation::Publication),
        )
        .route(
            "/v1/collections/{collection}/status",
            observed(get(status), &state, ServerOperation::Status),
        )
        .route(
            "/v1/collections/{collection}/search",
            observed(get(search), &state, ServerOperation::Search),
        )
        .route(
            "/v1/collections/{collection}/events/{citation}",
            observed(get(event), &state, ServerOperation::Event),
        )
        .route(
            "/v1/collections/{collection}/sessions/{citation}",
            observed(get(session), &state, ServerOperation::Session),
        )
        .layer(DefaultBodyLimit::max(body_limit))
        .fallback(unknown_route)
        .method_not_allowed_fallback(unknown_method)
        .with_state(state)
}

fn observed(
    route: MethodRouter<HttpState>,
    state: &HttpState,
    operation: ServerOperation,
) -> MethodRouter<HttpState> {
    route.route_layer(middleware::from_fn_with_state(
        (state.clone(), operation),
        admission,
    ))
}

async fn unknown_route(State(state): State<HttpState>) -> Response {
    unknown(state, ServerFailure::NotFound, StatusCode::NOT_FOUND)
}
async fn unknown_method(State(state): State<HttpState>) -> Response {
    unknown(state, ServerFailure::Method, StatusCode::METHOD_NOT_ALLOWED)
}
fn unknown(state: HttpState, failure: ServerFailure, status: StatusCode) -> Response {
    let _permit = state.requests.clone().try_acquire_owned();
    let (status, failure) = if _permit.is_err() {
        (
            StatusCode::TOO_MANY_REQUESTS,
            ServerFailure::RequestCapacity,
        )
    } else {
        (status, failure)
    };
    let response = if failure == ServerFailure::RequestCapacity {
        Error::Capacity.into_response()
    } else {
        status.into_response()
    };
    observe_response(
        &state.server,
        ServerOperation::Unknown,
        Instant::now(),
        response,
        Some(failure),
    )
}

async fn admission(
    State((state, operation)): State<(HttpState, ServerOperation)>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let is_head = request.method() == axum::http::Method::HEAD;
    let mut guard = RequestGuard::new(&state.server, operation, started);
    let permit = state.requests.clone().try_acquire_owned();
    let (mut response, failure) = match permit {
        Err(_) => (
            Error::Capacity.into_response(),
            Some(ServerFailure::RequestCapacity),
        ),
        Ok(_permit) => match tokio::time::timeout(state.request_timeout, next.run(request)).await {
            Ok(response) => {
                let failure = response
                    .extensions()
                    .get::<ServerFailure>()
                    .copied()
                    .or_else(|| match response.status().as_u16() {
                        200..=299 => None,
                        401 => Some(ServerFailure::Unauthorized),
                        403 => Some(ServerFailure::Forbidden),
                        404 => Some(ServerFailure::NotFound),
                        405 => Some(ServerFailure::Method),
                        413 => Some(ServerFailure::BodyTooLarge),
                        400 | 415 | 422 => Some(ServerFailure::Invalid),
                        _ => Some(ServerFailure::Other),
                    });
                (response, failure)
            }
            Err(_) => (
                StatusCode::REQUEST_TIMEOUT.into_response(),
                Some(ServerFailure::Timeout),
            ),
        },
    };
    if is_head {
        response.extensions_mut().insert(HeadResponse);
    }
    guard.disarm();
    observe_response(&state.server, operation, started, response, failure)
}

fn bearer(headers: &HeaderMap) -> Result<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(Error::Unauthorized)?;
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Unauthorized);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use std::sync::Mutex;
    use tower::ServiceExt;

    #[tokio::test]
    async fn request_timeout_and_request_capacity_do_not_claim_execution() {
        let root = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let capture = events.clone();
        let server = Arc::new(
            HistoryServer::open_with_observer(
                ServerConfig::new(root.path()),
                Some(Arc::new(move |event| capture.lock().unwrap().push(event))),
            )
            .unwrap(),
        );
        for capacity in [0, 1] {
            let state = HttpState {
                server: server.clone(),
                requests: Arc::new(Semaphore::new(capacity)),
                work: Arc::new(Semaphore::new(1)),
                request_timeout: Duration::ZERO,
            };
            let route = observed(
                get(|| async {
                    std::future::pending::<()>().await;
                    StatusCode::OK
                }),
                &state,
                ServerOperation::Search,
            );
            let app = Router::new().route("/test", route).with_state(state);
            let response = app
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/test")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if capacity == 0 {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::REQUEST_TIMEOUT
                }
            );
            let _ = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            let mut events = events.lock().unwrap();
            assert_eq!(events.len(), 1);
            assert!(
                matches!(events[0], ServerObservation::Request { failure: Some(failure), execution: None, read: None, body_handed_off: Some(true), .. } if failure == if capacity == 0 { ServerFailure::RequestCapacity } else { ServerFailure::Timeout })
            );
            events.clear();
        }
    }

    #[tokio::test]
    async fn exhausted_work_budget_reports_admission_without_executing_action() {
        let root = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let capture = events.clone();
        let server = Arc::new(
            HistoryServer::open_with_observer(
                ServerConfig::new(root.path()),
                Some(Arc::new(move |event| capture.lock().unwrap().push(event))),
            )
            .unwrap(),
        );
        let state = HttpState {
            server,
            requests: Arc::new(Semaphore::new(1)),
            work: Arc::new(Semaphore::new(0)),
            request_timeout: Duration::from_secs(30),
        };
        let response = blocking::<()>(state, ServerOperation::Search, |_| {
            panic!("saturated action must not execute")
        })
        .await
        .unwrap()
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.extensions().get::<ServerFailure>(),
            Some(&ServerFailure::WorkCapacity)
        );
        assert!(response.extensions().get::<ServerReadFacts>().is_none());
        assert!(matches!(
            events.lock().unwrap().as_slice(),
            [ServerObservation::Execution {
                operation: ServerOperation::Search,
                failure: Some(ServerFailure::WorkCapacity),
                ..
            }]
        ));
    }
}

async fn blocking<T: Serialize + Send + 'static>(
    state: HttpState,
    operation: ServerOperation,
    action: impl FnOnce(&HistoryServer) -> Result<T> + Send + 'static,
) -> Result<ObservedJson<T>> {
    blocking_observed(state, operation, move |server| {
        action(server).map(|value| (value, None))
    })
    .await
}

async fn blocking_read<T: Serialize + Send + 'static>(
    state: HttpState,
    operation: ServerOperation,
    action: impl FnOnce(&HistoryServer) -> Result<(T, ServerReadFacts)> + Send + 'static,
) -> Result<ObservedJson<T>> {
    blocking_observed(state, operation, move |server| {
        action(server).map(|(value, facts)| (value, Some(facts)))
    })
    .await
}

async fn blocking_observed<T: Serialize + Send + 'static>(
    state: HttpState,
    operation: ServerOperation,
    action: impl FnOnce(&HistoryServer) -> Result<(T, Option<ServerReadFacts>)> + Send + 'static,
) -> Result<ObservedJson<T>> {
    let started = Instant::now();
    // Work remains admitted until the blocking action finishes, even when
    // HTTP deadline/cancellation has already dropped its response receiver.
    let permit = match state.work.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            let execution = ServerExecutionFacts {
                duration: started.elapsed(),
                failure: Some(ServerFailure::WorkCapacity),
            };
            state.server.observe(ServerObservation::Execution {
                operation,
                duration: execution.duration,
                failure: execution.failure,
            });
            return Ok(ObservedJson {
                result: Err(Error::Capacity),
                execution,
                read: None,
            });
        }
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = action(&state.server);
        let execution = ServerExecutionFacts {
            duration: started.elapsed(),
            failure: result.as_ref().err().map(ServerFailure::from),
        };
        state.server.observe(ServerObservation::Execution {
            operation,
            duration: execution.duration,
            failure: execution.failure,
        });
        let (result, read) = match result {
            Ok((value, read)) => (Ok(value), read),
            Err(error) => (Err(error), None),
        };
        ObservedJson {
            result,
            execution,
            read,
        }
    })
    .await
    .map_err(|_| Error::Unavailable)
}

async fn enroll(
    State(state): State<HttpState>,
    Json(request): Json<EnrollRequest>,
) -> Result<ObservedJson<TokenFile>> {
    blocking(state, ServerOperation::Enroll, move |server| {
        server.redeem(&request.enrollment)
    })
    .await
}
async fn invite(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<InviteRequest>,
) -> Result<ObservedJson<EnrollmentFile>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Invite, move |server| {
        server.invite(&token, &collection, request)
    })
    .await
}
async fn grants(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<GrantRequest>,
) -> Result<ObservedJson<serde_json::Value>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Grants, move |server| {
        server.manage_grants(&token, &request.principal, &collection, request.grants)?;
        Ok(serde_json::json!({}))
    })
    .await
}
async fn revoke(
    State(state): State<HttpState>,
    Path((collection, principal)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<ObservedJson<serde_json::Value>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::RevokeMember, move |server| {
        server.revoke_member(&token, &collection, &principal)?;
        Ok(serde_json::json!({}))
    })
    .await
}
async fn whoami(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
) -> Result<ObservedJson<ConnectionIdentity>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Whoami, move |server| {
        server.whoami(&token, &collection)
    })
    .await
}
async fn principals(
    State(state): State<HttpState>,
    Query(request): Query<AccessListRequest>,
    headers: HeaderMap,
) -> Result<ObservedJson<PrincipalPage>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Principals, move |server| {
        server.list_principals(&token, request)
    })
    .await
}
async fn credentials(
    State(state): State<HttpState>,
    Path(principal): Path<String>,
    Query(request): Query<AccessListRequest>,
    headers: HeaderMap,
) -> Result<ObservedJson<CredentialPage>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Credentials, move |server| {
        server.list_credentials(&token, &principal, request)
    })
    .await
}
async fn revoke_principal(
    State(state): State<HttpState>,
    Path(principal): Path<String>,
    headers: HeaderMap,
) -> Result<ObservedJson<serde_json::Value>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::RevokePrincipal, move |server| {
        server.admin_revoke_principal(&token, &principal)?;
        Ok(serde_json::json!({}))
    })
    .await
}
async fn revoke_credential(
    State(state): State<HttpState>,
    Path(credential_id): Path<String>,
    headers: HeaderMap,
) -> Result<ObservedJson<serde_json::Value>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::RevokeCredential, move |server| {
        server.admin_revoke_credential(&token, &credential_id)?;
        Ok(serde_json::json!({}))
    })
    .await
}
async fn begin_upload(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(spec): Json<UploadSpec>,
) -> Result<ObservedJson<UploadStatus>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::BeginUpload, move |server| {
        server.begin_upload(&token, &collection, spec)
    })
    .await
}
async fn upload_status(
    State(state): State<HttpState>,
    Path((collection, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<ObservedJson<UploadStatus>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::UploadStatus, move |server| {
        server.upload_status(&token, &collection, &id)
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offset {
    offset: u64,
}
async fn upload_chunk(
    State(state): State<HttpState>,
    Path((collection, id)): Path<(String, String)>,
    Query(offset): Query<Offset>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<ObservedJson<UploadStatus>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::UploadChunk, move |server| {
        server.upload_chunk(&token, &collection, &id, offset.offset, &bytes)
    })
    .await
}
async fn publish(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PublishRequest>,
) -> Result<ObservedJson<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Publish, move |server| {
        server.publish(&token, &collection, request)
    })
    .await
}
async fn cancel_publish(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CancelPublishRequest>,
) -> Result<ObservedJson<CancelPublishResponse>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::CancelPublish, move |server| {
        server.cancel_publish(&token, &collection, request)
    })
    .await
}
async fn withdraw(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WithdrawRequest>,
) -> Result<ObservedJson<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Withdraw, move |server| {
        server.withdraw(&token, &collection, request)
    })
    .await
}
async fn remove(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WithdrawRequest>,
) -> Result<ObservedJson<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Remove, move |server| {
        server.remove_publication(&token, &collection, request)
    })
    .await
}
async fn receipt(
    State(state): State<HttpState>,
    Path((collection, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<ObservedJson<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Receipt, move |server| {
        server.receipt(&token, &collection, &id)
    })
    .await
}
async fn publication_state(
    State(state): State<HttpState>,
    Path((collection, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<ObservedJson<PublicationState>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Publication, move |server| {
        server.publication_state(&token, &collection, &id)
    })
    .await
}
async fn publications(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    Query(request): Query<PublicationListRequest>,
    headers: HeaderMap,
) -> Result<ObservedJson<PublicationPage>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Publications, move |server| {
        server.list_publications(&token, &collection, request)
    })
    .await
}
async fn status(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
) -> Result<ObservedJson<CollectionStatus>> {
    let token = bearer(&headers)?;
    blocking(state, ServerOperation::Status, move |server| {
        server.status(&token, &collection)
    })
    .await
}
async fn search(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    Query(request): Query<SearchRequest>,
    headers: HeaderMap,
) -> Result<ObservedJson<SearchResponse>> {
    let token = bearer(&headers)?;
    blocking_read(state, ServerOperation::Search, move |server| {
        server.search_with_facts(&token, &collection, request)
    })
    .await
}
async fn event(
    State(state): State<HttpState>,
    Path((collection, citation)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<ObservedJson<HostedEvent>> {
    let token = bearer(&headers)?;
    blocking_read(state, ServerOperation::Event, move |server| {
        server.read_event_with_facts(&token, &collection, &citation)
    })
    .await
}
async fn session(
    State(state): State<HttpState>,
    Path((collection, citation)): Path<(String, String)>,
    Query(request): Query<SessionRequest>,
    headers: HeaderMap,
) -> Result<ObservedJson<SessionPage>> {
    let token = bearer(&headers)?;
    blocking_read(state, ServerOperation::Session, move |server| {
        server.read_session_with_facts(&token, &collection, &citation, request)
    })
    .await
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let failure = ServerFailure::from(&self);
        let (status, code) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict => (StatusCode::CONFLICT, "conflict"),
            Self::OperationCancelled => (StatusCode::CONFLICT, "operation_cancelled"),
            Self::Expired => (StatusCode::GONE, "upload_expired"),
            Self::Invalid(_)
            | Self::Json(_)
            | Self::Archive(_)
            | Self::Core(_)
            | Self::Identity(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::Capacity => (StatusCode::TOO_MANY_REQUESTS, "capacity"),
            Self::Unavailable | Self::Index(_) => {
                (StatusCode::SERVICE_UNAVAILABLE, "search_unavailable")
            }
            Self::Io(_) | Self::Sql(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_failure"),
        };
        let mut response = (status, Json(serde_json::json!({"error":code}))).into_response();
        response.extensions_mut().insert(failure);
        response
    }
}

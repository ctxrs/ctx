use crate::*;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct HttpState {
    server: Arc<HistoryServer>,
    requests: Arc<Semaphore>,
    work: Arc<Semaphore>,
}

pub fn router(server: Arc<HistoryServer>) -> Router {
    let body_limit = server.config.max_chunk_bytes.max(256 * 1024);
    let state = HttpState {
        requests: Arc::new(Semaphore::new(server.config.max_in_flight)),
        work: Arc::new(Semaphore::new(server.config.max_in_flight)),
        server,
    };
    Router::new()
        .route(
            "/healthz",
            get(|| async { Json(serde_json::json!({"alive":true})) }),
        )
        .route("/v1/enroll", post(enroll))
        .route("/v1/collections/{collection}/invite", post(invite))
        .route("/v1/collections/{collection}/grants", post(grants))
        .route(
            "/v1/collections/{collection}/members/{principal}/revoke",
            post(revoke),
        )
        .route("/v1/collections/{collection}/uploads", post(begin_upload))
        .route(
            "/v1/collections/{collection}/uploads/{id}",
            get(upload_status).put(upload_chunk),
        )
        .route("/v1/collections/{collection}/revisions", post(publish))
        .route(
            "/v1/collections/{collection}/operations/cancel",
            post(cancel_publish),
        )
        .route("/v1/collections/{collection}/withdraw", post(withdraw))
        .route("/v1/collections/{collection}/remove", post(remove))
        .route("/v1/collections/{collection}/receipts/{id}", get(receipt))
        .route(
            "/v1/collections/{collection}/publications",
            get(publications),
        )
        .route(
            "/v1/collections/{collection}/publications/{id}",
            get(publication_state),
        )
        .route("/v1/collections/{collection}/status", get(status))
        .route("/v1/collections/{collection}/search", get(search))
        .route("/v1/collections/{collection}/events/{citation}", get(event))
        .route(
            "/v1/collections/{collection}/sessions/{citation}",
            get(session),
        )
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(middleware::from_fn_with_state(state.clone(), admission))
        .with_state(state)
}

pub fn serve_blocking(server: Arc<HistoryServer>) -> Result<()> {
    serve_blocking_with_ready(server, |_| Ok(()))
}

/// Run the callback with the actual bound address before accepting requests.
/// A failed bind skips it; callback failure closes the listener and returns.
pub fn serve_blocking_with_ready(
    server: Arc<HistoryServer>,
    ready: impl FnOnce(SocketAddr) -> Result<()>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(serve_with_ready(server, ready))
}

/// Plain HTTP is bound to loopback unless trusted TLS ingress was explicitly
/// selected. Authorization never trusts forwarded identity headers.
pub async fn serve(server: Arc<HistoryServer>) -> Result<()> {
    serve_with_ready(server, |_| Ok(())).await
}

async fn serve_with_ready(
    server: Arc<HistoryServer>,
    ready: impl FnOnce(SocketAddr) -> Result<()>,
) -> Result<()> {
    server.config.validate()?;
    let listener = tokio::net::TcpListener::bind(server.config.bind).await?;
    let address = listener.local_addr()?;
    ready(address)?;
    eprintln!("ctx history server listening on {address}");
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let index_server = server.clone();
    let indexer = tokio::spawn(async move {
        let mut reported_failure = false;
        let mut after = String::new();
        loop {
            tokio::select! {
                _=stopped.changed()=>break,
                _=tokio::time::sleep(Duration::from_millis(250))=>{}
            }
            let server = index_server.clone();
            let mut cursor = std::mem::take(&mut after);
            let result = tokio::task::spawn_blocking(move || {
                let result = server.index_sweep(&mut cursor);
                (result, cursor)
            })
            .await;
            let failed = match result {
                Ok((result, cursor)) => {
                    after = cursor;
                    result.is_err()
                }
                Err(_) => true,
            };
            if failed && !reported_failure {
                if let Ok(connection) = index_server.lock() {
                    let _ = crate::catalog::audit(&connection, "index_unavailable", None, None);
                }
            }
            reported_failure = failed;
        }
    });
    let served = axum::serve(listener, router(server))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    let _ = stop.send(true);
    let _ = indexer.await;
    served.map_err(Error::Io)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn admission(
    State(state): State<HttpState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Ok(_permit) = state.requests.try_acquire_owned() else {
        return Error::Capacity.into_response();
    };
    match tokio::time::timeout(Duration::from_secs(30), next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
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

async fn blocking<T: Serialize + Send + 'static>(
    state: HttpState,
    action: impl FnOnce(&HistoryServer) -> Result<T> + Send + 'static,
) -> Result<Json<T>> {
    // A timed-out request cannot release its work slot while its blocking
    // transaction is still running; retries therefore stay resource-bounded.
    let permit = state
        .work
        .try_acquire_owned()
        .map_err(|_| Error::Capacity)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        action(&state.server)
    })
    .await
    .map_err(|_| Error::Unavailable)?
    .map(Json)
}

async fn enroll(
    State(state): State<HttpState>,
    Json(request): Json<EnrollRequest>,
) -> Result<Json<TokenFile>> {
    blocking(state, move |server| server.redeem(&request.enrollment)).await
}
async fn invite(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<InviteRequest>,
) -> Result<Json<EnrollmentFile>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.invite(&token, &collection, request)
    })
    .await
}
async fn grants(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<GrantRequest>,
) -> Result<Json<serde_json::Value>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.manage_grants(&token, &request.principal, &collection, request.grants)?;
        Ok(serde_json::json!({}))
    })
    .await
}
async fn revoke(
    State(state): State<HttpState>,
    Path((collection, principal)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.revoke_member(&token, &collection, &principal)?;
        Ok(serde_json::json!({}))
    })
    .await
}
async fn begin_upload(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(spec): Json<UploadSpec>,
) -> Result<Json<UploadStatus>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.begin_upload(&token, &collection, spec)
    })
    .await
}
async fn upload_status(
    State(state): State<HttpState>,
    Path((collection, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<UploadStatus>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
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
) -> Result<Json<UploadStatus>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.upload_chunk(&token, &collection, &id, offset.offset, &bytes)
    })
    .await
}
async fn publish(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PublishRequest>,
) -> Result<Json<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.publish(&token, &collection, request)
    })
    .await
}
async fn cancel_publish(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CancelPublishRequest>,
) -> Result<Json<CancelPublishResponse>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.cancel_publish(&token, &collection, request)
    })
    .await
}
async fn withdraw(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WithdrawRequest>,
) -> Result<Json<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.withdraw(&token, &collection, request)
    })
    .await
}
async fn remove(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WithdrawRequest>,
) -> Result<Json<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.remove_publication(&token, &collection, request)
    })
    .await
}
async fn receipt(
    State(state): State<HttpState>,
    Path((collection, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Receipt>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.receipt(&token, &collection, &id)
    })
    .await
}
async fn publication_state(
    State(state): State<HttpState>,
    Path((collection, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<PublicationState>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.publication_state(&token, &collection, &id)
    })
    .await
}
async fn publications(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    Query(request): Query<PublicationListRequest>,
    headers: HeaderMap,
) -> Result<Json<PublicationPage>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.list_publications(&token, &collection, request)
    })
    .await
}
async fn status(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CollectionStatus>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| server.status(&token, &collection)).await
}
async fn search(
    State(state): State<HttpState>,
    Path(collection): Path<String>,
    Query(request): Query<SearchRequest>,
    headers: HeaderMap,
) -> Result<Json<SearchResponse>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.search(&token, &collection, request)
    })
    .await
}
async fn event(
    State(state): State<HttpState>,
    Path((collection, citation)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<HostedEvent>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.read_event(&token, &collection, &citation)
    })
    .await
}
async fn session(
    State(state): State<HttpState>,
    Path((collection, citation)): Path<(String, String)>,
    Query(request): Query<SessionRequest>,
    headers: HeaderMap,
) -> Result<Json<SessionPage>> {
    let token = bearer(&headers)?;
    blocking(state, move |server| {
        server.read_session(&token, &collection, &citation, request)
    })
    .await
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
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
        (status, Json(serde_json::json!({"error":code}))).into_response()
    }
}

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

#[derive(Clone)]
struct StubState {
    fixtures: Arc<Mutex<VecDeque<String>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

pub struct OpenAiResponsesSseStub {
    pub base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    handle: JoinHandle<()>,
}

impl OpenAiResponsesSseStub {
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for OpenAiResponsesSseStub {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

pub async fn spawn_openai_responses_sse_stub(fixtures: Vec<String>) -> OpenAiResponsesSseStub {
    let state = StubState {
        fixtures: Arc::new(Mutex::new(VecDeque::from(fixtures))),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let requests = Arc::clone(&state.requests);

    let app = Router::new()
        .route("/v1/models", get(handle_models))
        .route("/v1/responses", post(handle_responses))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    OpenAiResponsesSseStub {
        base_url: format!("http://{addr}"),
        requests,
        handle,
    }
}

pub fn load_fixture(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).expect("fixture read failed")
}

pub fn parse_sse_events(sse: &str) -> Vec<Value> {
    sse.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|json| serde_json::from_str(json).ok())
        .collect()
}

async fn handle_models() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": [{"id": "mock-model", "object": "model"}],
        })),
    )
}

async fn handle_responses(
    State(state): State<StubState>,
    Json(payload): Json<Value>,
) -> Response<Body> {
    state.requests.lock().unwrap().push(payload);
    let body = state
        .fixtures
        .lock()
        .unwrap()
        .pop_front()
        .expect("no SSE fixtures left for /v1/responses");

    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    resp
}

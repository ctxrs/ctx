use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde_json::json;

fn print_help() {
    println!(
        "llama-server mock\n\nUSAGE:\n  llama-server [OPTIONS]\n\nOPTIONS:\n  --model <MODEL>\n  --host <HOST>\n  --port <PORT>\n  --json-schema-file <PATH>\n  --json-schema <SCHEMA>\n  --grammar-file <PATH>\n  --grammar <GRAMMAR>\n  -h, --help\n"
    );
}

fn parse_args(args: &[String]) -> (String, u16) {
    let mut host = "127.0.0.1".to_string();
    let mut port = 0u16;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                if let Some(value) = args.get(i + 1) {
                    host = value.clone();
                    i += 1;
                }
            }
            "--port" => {
                if let Some(value) = args.get(i + 1) {
                    port = value.parse().unwrap_or(0);
                    i += 1;
                }
            }
            "--model" | "--json-schema-file" | "--json-schema" | "--grammar" | "--grammar-file"
                if args.get(i + 1).is_some() =>
            {
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    (host, port)
}

async fn health() -> &'static str {
    "ok"
}

async fn models() -> Json<serde_json::Value> {
    Json(json!({"data": []}))
}

async fn chat_completions() -> Json<serde_json::Value> {
    Json(json!({
        "choices": [
            {
                "message": {
                    "content": "{\"title\":\"Mock Title\"}"
                }
            }
        ]
    }))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return;
    }

    let (host, port) = parse_args(&args);
    if port == 0 {
        eprintln!("--port is required");
        std::process::exit(1);
    }

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"));

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions));

    axum::serve(listener, app)
        .await
        .unwrap_or_else(|err| panic!("mock llama-server exited: {err}"));
}

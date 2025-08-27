use axum::{
    body::Body,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use outlet::{types::*, *};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, time::sleep};
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use tracing::{info, Level};

/// Custom handler that stores captured requests/responses in memory for demonstration
#[derive(Debug, Clone)]
struct DemoHandler {
    captured_data: Arc<Mutex<Vec<CapturedRequest>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapturedRequest {
    correlation_id: u64,
    method: String,
    uri: String,
    request_body: Option<String>,
    response_status: Option<u16>,
    response_body: Option<String>,
    duration_ms: Option<u64>,
    completed: bool,
}

impl DemoHandler {
    fn new() -> Self {
        Self {
            captured_data: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn get_captured_data(&self) -> Vec<CapturedRequest> {
        self.captured_data.lock().unwrap().clone()
    }
}

impl RequestHandler for DemoHandler {
    fn handle_request(&self, data: RequestData, correlation_id: u64) {
        let mut captured = self.captured_data.lock().unwrap();
        captured.push(CapturedRequest {
            correlation_id,
            method: data.method.to_string(),
            uri: data.uri.to_string(),
            request_body: data
                .body
                .map(|b| String::from_utf8_lossy(b.as_ref()).to_string()),
            response_status: None,
            response_body: None,
            duration_ms: None,
            completed: false,
        });
        info!("Captured request: {} {}", data.method, data.uri);
    }

    fn handle_response(&self, data: ResponseData, correlation_id: u64) {
        let mut captured = self.captured_data.lock().unwrap();
        if let Some(req) = captured
            .iter_mut()
            .find(|r| r.correlation_id == correlation_id)
        {
            req.response_status = Some(data.status.as_u16());
            req.response_body = data
                .body
                .map(|b| String::from_utf8_lossy(b.as_ref()).to_string());
            req.duration_ms = Some(data.duration.as_millis() as u64);
            req.completed = true;
            info!(
                "Completed request/response pair: {} {} -> {} ({}ms)",
                req.method,
                req.uri,
                data.status,
                data.duration.as_millis()
            );
        } else {
            info!(
                "Orphaned response: {} ({}ms)",
                data.status,
                data.duration.as_millis()
            );
        }
    }
}

// Test handlers for our demo server
async fn hello_handler() -> impl IntoResponse {
    sleep(Duration::from_millis(100)).await; // Simulate some work
    "Hello, World!"
}

async fn echo_handler(body: Bytes) -> impl IntoResponse {
    sleep(Duration::from_millis(50)).await; // Simulate some work
    format!("Echo: {}", String::from_utf8_lossy(&body))
}

async fn streaming_handler() -> impl IntoResponse {
    use futures::stream;
    use tokio::time::interval;

    let stream = stream::unfold(0u32, |count| async move {
        if count >= 5 {
            None
        } else {
            let mut interval = interval(Duration::from_millis(200));
            interval.tick().await;
            Some((
                Ok::<_, std::convert::Infallible>(Bytes::from(format!("chunk-{count}\n"))),
                count + 1,
            ))
        }
    });

    Response::builder()
        .header("content-type", "text/plain")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn large_response_handler() -> impl IntoResponse {
    // Generate a large response to test size limits
    let large_content = "x".repeat(2048); // 2KB of data
    sleep(Duration::from_millis(10)).await;
    large_content
}

async fn stats_handler(demo_handler: Arc<DemoHandler>) -> impl IntoResponse {
    let data = demo_handler.get_captured_data();
    axum::Json(serde_json::json!({
        "total_requests": data.len(),
        "completed_requests": data.iter().filter(|r| r.completed).count(),
        "requests": data
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    info!("Starting request middleware demo server");

    // Create our custom handler
    let demo_handler = Arc::new(DemoHandler::new());
    let demo_handler_clone = demo_handler.clone();

    // Configure middleware with custom settings
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
    };

    // Build the router with middleware
    let app = Router::new()
        .route("/hello", get(hello_handler))
        .route("/echo", post(echo_handler))
        .route("/streaming", get(streaming_handler))
        .route("/large", get(large_response_handler))
        .route(
            "/stats",
            get(move || stats_handler(demo_handler_clone.clone())),
        )
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(RequestLoggerLayer::new(
                    config,
                    demo_handler.as_ref().clone(),
                ))
                .into_inner(),
        );

    info!("Demo server endpoints:");
    info!("  GET  /hello      - Simple greeting");
    info!("  POST /echo       - Echo request body");
    info!("  GET  /streaming  - Streaming response");
    info!("  GET  /large      - Large response (tests size limits)");
    info!("  GET  /stats      - View captured request/response data");
    info!("");
    info!("Try these commands:");
    info!("  curl http://localhost:3000/hello");
    info!("  curl -X POST -d 'Hello from client' http://localhost:3000/echo");
    info!("  curl http://localhost:3000/streaming");
    info!("  curl http://localhost:3000/large");
    info!("  curl http://localhost:3000/stats");

    let listener = TcpListener::bind("0.0.0.0:3000").await?;
    info!("Demo server listening on http://localhost:3000");

    axum::serve(listener, app).await?;

    Ok(())
}

use axum::{
    body::Body,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use futures::stream;
use outlet::{types::*, RequestHandler, RequestLoggerConfig, RequestLoggerLayer};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::time::sleep;
use tower::ServiceBuilder;

use std::collections::HashMap;

/// Test handler that collects all captured data for verification
#[derive(Debug, Clone)]
struct TestHandler {
    requests: Arc<Mutex<Vec<(RequestData, u64)>>>,
    responses: Arc<Mutex<Vec<(ResponseData, u64)>>>,
    completed_pairs: Arc<Mutex<HashMap<u64, (RequestData, ResponseData)>>>,
}

impl TestHandler {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(Vec::new())),
            completed_pairs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn get_requests(&self) -> Vec<(RequestData, u64)> {
        self.requests.lock().unwrap().clone()
    }

    fn get_responses(&self) -> Vec<(ResponseData, u64)> {
        self.responses.lock().unwrap().clone()
    }

    fn get_completed_pairs(&self) -> Vec<(u64, RequestData, ResponseData)> {
        let pairs = self.completed_pairs.lock().unwrap();
        pairs
            .iter()
            .map(|(id, (req, resp))| (*id, req.clone(), resp.clone()))
            .collect()
    }

    fn wait_for_pairs(&self, expected_count: usize, timeout: Duration) -> bool {
        let start = SystemTime::now();
        while start.elapsed().unwrap() < timeout {
            if self.completed_pairs.lock().unwrap().len() >= expected_count {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

impl RequestHandler for TestHandler {
    async fn handle_request(&self, data: RequestData) {
        self.requests
            .lock()
            .unwrap()
            .push((data.clone(), data.correlation_id));
    }

    async fn handle_response(&self, request_data: RequestData, response_data: ResponseData) {
        self.responses
            .lock()
            .unwrap()
            .push((response_data.clone(), request_data.correlation_id));

        // Create a completed pair
        self.completed_pairs
            .lock()
            .unwrap()
            .insert(request_data.correlation_id, (request_data, response_data));
    }
}

// Test server handlers
async fn hello_handler() -> impl IntoResponse {
    "Hello, World!"
}

async fn echo_handler(body: Bytes) -> impl IntoResponse {
    format!("Echo: {}", String::from_utf8_lossy(&body))
}

async fn delayed_handler() -> impl IntoResponse {
    sleep(Duration::from_millis(100)).await;
    "Delayed response"
}

async fn streaming_handler() -> impl IntoResponse {
    let stream = stream::iter(vec![
        Ok::<_, std::convert::Infallible>(Bytes::from("chunk1")),
        Ok(Bytes::from("chunk2")),
        Ok(Bytes::from("chunk3")),
    ]);

    Response::builder()
        .header("content-type", "text/plain")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn large_handler() -> impl IntoResponse {
    "x".repeat(2048) // 2KB
}

fn create_test_app(handler: TestHandler, config: RequestLoggerConfig) -> Router {
    Router::new()
        .route("/hello", get(hello_handler))
        .route("/echo", post(echo_handler))
        .route("/delayed", get(delayed_handler))
        .route("/streaming", get(streaming_handler))
        .route("/large", get(large_handler))
        .layer(
            ServiceBuilder::new()
                .layer(RequestLoggerLayer::new(config, handler))
                .into_inner(),
        )
}

#[tokio::test]
async fn test_basic_request_response() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: false,
        capture_response_body: false,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    let response = server.get("/hello").await;
    assert_eq!(response.status_code(), StatusCode::OK);
    assert_eq!(response.text(), "Hello, World!");

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(1, Duration::from_secs(1)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 1);

    let (_correlation_id, request, response) = &pairs[0];
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.uri.path(), "/hello");
    assert_eq!(response.status, StatusCode::OK);
    assert!(response.duration > Duration::ZERO);
}

#[tokio::test]
async fn test_request_with_body_capture() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    let test_body = "Hello, World!";
    let response = server.post("/echo").text(test_body).await;
    assert_eq!(response.status_code(), StatusCode::OK);
    assert_eq!(response.text(), format!("Echo: {test_body}"));

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(1, Duration::from_secs(1)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 1);

    let (_correlation_id, request, response) = &pairs[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.uri.path(), "/echo");

    // Check request body was captured
    assert!(request.body.is_some());
    assert_eq!(
        String::from_utf8_lossy(request.body.as_ref().unwrap()),
        test_body
    );

    // Check response body was captured
    assert!(response.body.is_some());
    assert_eq!(
        String::from_utf8_lossy(response.body.as_ref().unwrap()),
        format!("Echo: {test_body}")
    );
}

#[tokio::test]
async fn test_streaming_response_capture() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    let response = server.get("/streaming").await;
    assert_eq!(response.status_code(), StatusCode::OK);
    assert_eq!(response.text(), "chunk1chunk2chunk3");

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(1, Duration::from_secs(1)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 1);

    let (_correlation_id, _request, response) = &pairs[0];
    assert_eq!(response.status, StatusCode::OK);

    // Check that streaming response was captured correctly
    assert!(response.body.is_some());
    assert_eq!(
        String::from_utf8_lossy(response.body.as_ref().unwrap()),
        "chunk1chunk2chunk3"
    );
}

#[tokio::test]
async fn test_large_body_size_limit() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    let response = server.get("/large").await;
    assert_eq!(response.status_code(), StatusCode::OK);

    let full_response = response.text();
    assert_eq!(full_response.len(), 2048); // Full response should come through

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(1, Duration::from_secs(1)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 1);

    let (_correlation_id, _request, response) = &pairs[0];

    // Response body should be captured completely (no size limit anymore)
    if let Some(captured_body) = &response.body {
        assert_eq!(captured_body.len(), 2048); // Full response should be captured
        assert_eq!(String::from_utf8_lossy(captured_body), "x".repeat(2048));
    }
}

#[tokio::test]
async fn test_multiple_concurrent_requests() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
    };
    let app = create_test_app(handler.clone(), config);
    let server = std::sync::Arc::new(axum_test::TestServer::new(app).unwrap());

    // Send multiple concurrent requests using futures
    use futures::future::join_all;

    let futures: Vec<_> = (0..5)
        .map(|i| {
            let server = server.clone();
            async move { server.post("/echo").text(format!("Request {i}")).await }
        })
        .collect();

    // Wait for all requests to complete concurrently
    let responses = join_all(futures).await;

    // Verify all responses are correct
    for (i, response) in responses.iter().enumerate() {
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(response.text(), format!("Echo: Request {i}"));
    }

    // Wait for background processing of all pairs
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(5, Duration::from_secs(2)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 5);

    // Verify each request was captured with unique correlation ID
    let mut correlation_ids = std::collections::HashSet::new();
    for (correlation_id, request, response) in &pairs {
        assert!(correlation_ids.insert(*correlation_id));
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.uri.path(), "/echo");
        assert_eq!(response.status, StatusCode::OK);

        // Verify body content matches
        assert!(request.body.is_some());
        assert!(response.body.is_some());

        let request_body = String::from_utf8_lossy(request.body.as_ref().unwrap());
        let response_body = String::from_utf8_lossy(response.body.as_ref().unwrap());
        assert_eq!(response_body, format!("Echo: {request_body}"));
    }
}

#[tokio::test]
async fn test_timing_accuracy() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: false,
        capture_response_body: false,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    let start_time = SystemTime::now();
    let response = server.get("/delayed").await;
    let end_time = SystemTime::now();

    assert_eq!(response.status_code(), StatusCode::OK);
    assert_eq!(response.text(), "Delayed response");

    let actual_duration = end_time.duration_since(start_time).unwrap();
    assert!(actual_duration >= Duration::from_millis(100));

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(1, Duration::from_secs(1)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 1);

    let (_correlation_id, _request, response) = &pairs[0];
    // The captured duration should be approximately the same as actual duration
    assert!(response.duration >= Duration::from_millis(90)); // Allow some tolerance
    assert!(response.duration <= actual_duration + Duration::from_millis(50));
}

#[tokio::test]
async fn test_empty_body_handling() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    // GET request with empty body
    let response = server.get("/hello").await;
    assert_eq!(response.status_code(), StatusCode::OK);

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(1, Duration::from_secs(1)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 1);

    let (_correlation_id, request, response) = &pairs[0];
    // Empty request body should be handled gracefully
    assert!(request.body.is_none() || request.body.as_ref().unwrap().is_empty());

    // Response should still be captured
    assert!(response.body.is_some());
    assert_eq!(
        String::from_utf8_lossy(response.body.as_ref().unwrap()),
        "Hello, World!"
    );
}

#[tokio::test]
async fn test_correlation_isolation() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    // Send different types of requests
    let hello_response = server.get("/hello").await;
    let echo_response = server.post("/echo").text("test message").await;
    let delayed_response = server.get("/delayed").await;

    assert_eq!(hello_response.status_code(), StatusCode::OK);
    assert_eq!(echo_response.status_code(), StatusCode::OK);
    assert_eq!(delayed_response.status_code(), StatusCode::OK);

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(3, Duration::from_secs(2)));

    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 3);

    // Verify each request type was handled correctly
    let hello_pair = pairs
        .iter()
        .find(|(_, req, _)| req.uri.path() == "/hello")
        .unwrap();
    let echo_pair = pairs
        .iter()
        .find(|(_, req, _)| req.uri.path() == "/echo")
        .unwrap();
    let delayed_pair = pairs
        .iter()
        .find(|(_, req, _)| req.uri.path() == "/delayed")
        .unwrap();

    assert_eq!(hello_pair.1.method, Method::GET);
    assert_eq!(echo_pair.1.method, Method::POST);
    assert_eq!(delayed_pair.1.method, Method::GET);

    // Verify correlation IDs are unique
    let mut correlation_ids = std::collections::HashSet::new();
    for (correlation_id, _, _) in &pairs {
        assert!(correlation_ids.insert(*correlation_id));
    }
    assert_eq!(correlation_ids.len(), 3);

    // Verify timing differences
    assert!(delayed_pair.2.duration > hello_pair.2.duration);
    assert!(delayed_pair.2.duration >= Duration::from_millis(90));
}

#[tokio::test]
async fn test_config_disable_capture() {
    let handler = TestHandler::new();
    let config = RequestLoggerConfig {
        capture_request_body: false,
        capture_response_body: false,
    };
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    let response = server.post("/echo").text("test body").await;
    assert_eq!(response.status_code(), StatusCode::OK);

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(1, Duration::from_secs(1)));

    let requests = handler.get_requests();
    let responses = handler.get_responses();
    let pairs = handler.get_completed_pairs();

    assert_eq!(requests.len(), 1);
    assert_eq!(responses.len(), 1); // Should have matching response
    assert_eq!(pairs.len(), 1);

    // Bodies should not be captured
    assert!(requests[0].0.body.is_none());
    assert!(pairs[0].1.body.is_none());
    assert!(pairs[0].2.body.is_none());
}

#[tokio::test]
async fn test_middleware_passthrough() {
    // Verify middleware doesn't interfere with normal operation
    let handler = TestHandler::new();
    let config = RequestLoggerConfig::default();
    let app = create_test_app(handler.clone(), config);
    let server = axum_test::TestServer::new(app).unwrap();

    // Test various endpoints
    let hello_response = server.get("/hello").await;
    assert_eq!(hello_response.status_code(), StatusCode::OK);
    assert_eq!(hello_response.text(), "Hello, World!");

    let echo_response = server.post("/echo").text("test").await;
    assert_eq!(echo_response.status_code(), StatusCode::OK);
    assert_eq!(echo_response.text(), "Echo: test");

    let streaming_response = server.get("/streaming").await;
    assert_eq!(streaming_response.status_code(), StatusCode::OK);
    assert_eq!(streaming_response.text(), "chunk1chunk2chunk3");

    // Wait for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(handler.wait_for_pairs(3, Duration::from_secs(2)));

    // Verify all were captured
    let pairs = handler.get_completed_pairs();
    assert_eq!(pairs.len(), 3);
}

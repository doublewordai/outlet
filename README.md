# Outlet

A high-performance Axum middleware for capturing and correlating HTTP requests and responses with full streaming support.

## Features

- ✅ **Stream-aware**: Handles streaming request and response bodies without blocking
- ✅ **Zero head-of-line blocking**: Never interferes with the natural request/response flow
- ✅ **Request/Response correlation**: Matches requests with their corresponding responses using unique correlation IDs
- ✅ **Configurable body capture**: Control whether to capture request/response bodies
- ✅ **Background processing**: All capture and processing happens asynchronously
- ✅ **Extensible**: Custom handlers for processing captured data

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
outlet = "0.1.0"
axum = "0.8"
tokio = { version = "1.0", features = ["full"] }
tower = "0.5"
```

Here's a practical example that tracks API usage metrics:

```rust
use axum::{routing::get, Router, Json};
use outlet::{RequestLoggerLayer, RequestLoggerConfig, RequestHandler, RequestData, ResponseData};
use tower::ServiceBuilder;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use serde_json::json;

// Track API usage metrics
#[derive(Debug, Default)]
struct MetricsHandler {
    stats: Arc<Mutex<HashMap<String, u64>>>,
}

impl RequestHandler for MetricsHandler {
    fn handle_request(&self, data: RequestData, _correlation_id: u64) {
        // Count requests by endpoint
        let endpoint = data.uri.path().to_string();
        let mut stats = self.stats.lock().unwrap();
        *stats.entry(endpoint).or_insert(0) += 1;
    }

    fn handle_response(&self, data: ResponseData, _correlation_id: u64) {
        // Log slow requests for monitoring
        if data.duration.as_millis() > 1000 {
            println!("SLOW REQUEST: {} took {}ms", 
                     data.status, data.duration.as_millis());
        }
    }
}

async fn hello() -> &'static str {
    "Hello, World!"
}

async fn stats(metrics: axum::extract::State<Arc<Mutex<HashMap<String, u64>>>>) -> Json<serde_json::Value> {
    let stats = metrics.lock().unwrap().clone();
    Json(json!({ "request_counts": stats }))
}

#[tokio::main]
async fn main() {
    let metrics = Arc::new(Mutex::new(HashMap::new()));
    let handler = MetricsHandler { stats: metrics.clone() };
    let layer = RequestLoggerLayer::new(RequestLoggerConfig::default(), handler);

    let app = Router::new()
        .route("/hello", get(hello))
        .route("/stats", get(stats))
        .with_state(metrics)
        .layer(ServiceBuilder::new().layer(layer));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

Now your API automatically tracks request counts and identifies slow requests!

## Configuration

Customize the middleware behavior:

```rust
use outlet::{RequestLoggerConfig, RequestLoggerLayer, LoggingHandler};

let config = RequestLoggerConfig {
    capture_request_body: true,     // Whether to capture request bodies
    capture_response_body: true,    // Whether to capture response bodies
};

let handler = LoggingHandler;
let layer = RequestLoggerLayer::new(config, handler);
```

## Custom Handlers

Implement your own request/response processing logic:

```rust
use outlet::{RequestHandler, RequestData, ResponseData};

#[derive(Debug)]
struct CustomHandler;

impl RequestHandler for CustomHandler {
    fn handle_request(&self, data: RequestData, correlation_id: u64) {
        println!("Request: {} {}", data.method, data.uri);
        // Custom processing logic here
    }

    fn handle_response(&self, data: ResponseData, correlation_id: u64) {
        println!("Response: {} ({}ms)", data.status, data.duration.as_millis());
        // Custom processing logic here  
    }
}

// Use with middleware
let config = RequestLoggerConfig::default();
let handler = CustomHandler;
let layer = RequestLoggerLayer::new(config, handler);
```

## Streaming Support

The middleware fully supports streaming requests and responses:

```rust
use axum::{body::Body, response::Response};
use futures::stream;
use bytes::Bytes;

async fn streaming_response() -> Response {
    let stream = stream::iter(vec![
        Ok::<_, std::convert::Infallible>(Bytes::from("chunk1\n")),
        Ok(Bytes::from("chunk2\n")), 
        Ok(Bytes::from("chunk3\n")),
    ]);
    
    Response::builder()
        .header("content-type", "text/plain")
        .body(Body::from_stream(stream))
        .unwrap()
}
```

The middleware will:

- ✅ Capture each chunk as it flows through
- ✅ Never block the stream waiting for completion  
- ✅ Correlate the final assembled body with the original request
- ✅ Handle backpressure correctly

## Data Types

### RequestData

```rust
pub struct RequestData {
    pub correlation_id: u64,       // Unique request identifier
    pub timestamp: SystemTime,     // When request was received
    pub method: Method,            // HTTP method
    pub uri: Uri,                  // Request URI
    pub headers: HeaderMap,        // Request headers
    pub body: Option<Bytes>,       // Request body (if captured)
}
```

### ResponseData

```rust
pub struct ResponseData {
    pub correlation_id: u64,       // Matches the request
    pub timestamp: SystemTime,     // When response was sent
    pub status: StatusCode,        // HTTP status code
    pub headers: HeaderMap,        // Response headers  
    pub body: Option<Bytes>,       // Response body (if captured)
    pub duration: Duration,        // Time from request to response
}
```

## Performance Characteristics

- **Zero-copy streaming**: Bodies are streamed through without buffering the entire content
- **Minimal latency impact**: Background processing doesn't block request handling
- **Memory efficient**: Optional body capture and streaming design prevent unbounded memory usage
- **High throughput**: Concurrent request handling with proper correlation

## Use Cases

- **API logging and monitoring**: Track all requests/responses through your API
- **Audit trails**: Maintain detailed records of system interactions  
- **Performance monitoring**: Measure request/response times and sizes
- **Debugging**: Capture request/response data for troubleshooting
- **Analytics**: Feed data to external analytics systems

## Running the Demo

```bash
cd outlet
cargo run --example demo

# In another terminal:
curl http://localhost:3000/hello
curl -X POST -d 'Hello there!' http://localhost:3000/echo  
curl http://localhost:3000/streaming
```

## Running Tests

```bash
cargo test
```

## Documentation

Generate and view the full API documentation:

```bash
cargo doc --open
```

## License

This project is licensed under the MIT License.
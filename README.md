# Outlet

A high-performance Axum middleware for relaying axum requests & responses to a background thread for asynchronous processing, with full streaming support, and minimal performance overhead.

## Features

- **Stream-aware**: Handles streaming request and response bodies without blocking
- **Configurable body capture**: Control whether to capture request/response bodies
- **Background processing**: All capture and processing happens asynchronously
- **Extensible**: Custom handlers for processing captured data

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

Implement the [`RequestHandler`] trait to create custom processing logic:

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
```


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
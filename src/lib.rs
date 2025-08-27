//! # Outlet
//!
//! A high-performance Axum middleware for relaying axum requests & responses to a background
//! thread for asynchronous processing, with full streaming support, and minimal performance
//! overhead.
//!
//! ## Features
//!
//! - **Stream-aware**: Handles streaming request and response bodies without blocking
//! - **Configurable body capture**: Control whether to capture request/response bodies
//! - **Background processing**: All capture and processing happens asynchronously
//! - **Extensible**: Custom handlers for processing captured data
//!
//! ## Quick Start
//!
//! Here's a practical example that tracks API usage metrics:
//!
//! ```rust,no_run
//! use axum::{routing::get, Router, Json};
//! use outlet::{RequestLoggerLayer, RequestLoggerConfig, RequestHandler, RequestData, ResponseData};
//! use tower::ServiceBuilder;
//! use std::sync::{Arc, Mutex};
//! use std::collections::HashMap;
//! use serde_json::json;
//!
//! // Track API usage metrics
//! #[derive(Debug, Default)]
//! struct MetricsHandler {
//!     stats: Arc<Mutex<HashMap<String, u64>>>,
//! }
//!
//! impl RequestHandler for MetricsHandler {
//!     fn handle_request(&self, data: RequestData, _correlation_id: u64) {
//!         // Count requests by endpoint
//!         let endpoint = data.uri.path().to_string();
//!         let mut stats = self.stats.lock().unwrap();
//!         *stats.entry(endpoint).or_insert(0) += 1;
//!     }
//!
//!     fn handle_response(&self, data: ResponseData, _correlation_id: u64) {
//!         // Log slow requests for monitoring
//!         if data.duration.as_millis() > 1000 {
//!             println!("SLOW REQUEST: {} took {}ms",
//!                      data.status, data.duration.as_millis());
//!         }
//!     }
//! }
//!
//! async fn hello() -> &'static str {
//!     "Hello, World!"
//! }
//!
//! async fn stats(metrics: axum::extract::State<Arc<Mutex<HashMap<String, u64>>>>) -> Json<serde_json::Value> {
//!     let stats = metrics.lock().unwrap().clone();
//!     Json(json!({ "request_counts": stats }))
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let metrics = Arc::new(Mutex::new(HashMap::new()));
//!     let handler = MetricsHandler { stats: metrics.clone() };
//!     let layer = RequestLoggerLayer::new(RequestLoggerConfig::default(), handler);
//!
//!     let app = Router::new()
//!         .route("/hello", get(hello))
//!         .route("/stats", get(stats))
//!         .with_state(metrics)
//!         .layer(ServiceBuilder::new().layer(layer));
//!
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
//!     axum::serve(listener, app).await.unwrap();
//! }
//! ```
//!
//! ## Custom Handlers
//!
//! Implement the [`RequestHandler`] trait to create custom processing logic:
//!
//! ```rust
//! use outlet::{RequestHandler, RequestData, ResponseData};
//!
//! #[derive(Debug)]
//! struct CustomHandler;
//!
//! impl RequestHandler for CustomHandler {
//!     fn handle_request(&self, data: RequestData, correlation_id: u64) {
//!         println!("Request: {} {}", data.method, data.uri);
//!         // Custom processing logic here
//!     }
//!
//!     fn handle_response(&self, data: ResponseData, correlation_id: u64) {
//!         println!("Response: {} ({}ms)", data.status, data.duration.as_millis());
//!         // Custom processing logic here  
//!     }
//! }
//! ```

use axum::{body::Body, extract::Request, response::Response};
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::SystemTime,
};
use tokio::sync::mpsc;
use tower::{Layer, Service};
use tracing::{debug, error, instrument};

pub mod types;
use types::BackgroundTask;
pub use types::{RequestData, ResponseData};

pub mod body_wrapper;
use body_wrapper::create_body_capture_stream;

pub mod logging_handler;
pub use logging_handler::LoggingHandler;

/// Global atomic counter for correlation IDs
static CORRELATION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Configuration for the request logging middleware.
///
/// Controls what data is captured and how the middleware behaves.
///
/// # Examples
///
/// ```rust
/// use outlet::RequestLoggerConfig;
///
/// // Default configuration
/// let config = RequestLoggerConfig::default();
///
/// // Custom configuration
/// let config = RequestLoggerConfig {
///     capture_request_body: true,
///     capture_response_body: false,
/// };
/// ```
#[derive(Clone, Debug)]
pub struct RequestLoggerConfig {
    /// Whether to capture request bodies
    pub capture_request_body: bool,
    /// Whether to capture response bodies
    pub capture_response_body: bool,
}

impl Default for RequestLoggerConfig {
    fn default() -> Self {
        Self {
            capture_request_body: true,
            capture_response_body: true,
        }
    }
}

/// Trait for handling captured request and response data.
///
/// Implement this trait to create custom logic for processing HTTP requests and responses
/// captured by the middleware. The trait provides separate methods for handling requests
/// and responses, both of which include a correlation ID to match them together.
///
/// # Examples
///
/// ```rust
/// use outlet::{RequestHandler, RequestData, ResponseData};
/// use tracing::info;
///
/// #[derive(Debug)]
/// struct MyHandler;
///
/// impl RequestHandler for MyHandler {
///     fn handle_request(&self, data: RequestData, correlation_id: u64) {
///         info!("Received {} request to {}", data.method, data.uri);
///     }
///
///     fn handle_response(&self, data: ResponseData, correlation_id: u64) {
///         info!("Sent {} response in {}ms",
///               data.status, data.duration.as_millis());
///     }
/// }
/// ```
pub trait RequestHandler: Send + Sync + 'static {
    /// Handle a captured HTTP request.
    ///
    /// This method is called when a request has been captured by the middleware.
    /// The `correlation_id` can be used to match this request with its corresponding
    /// response in [`handle_response`](Self::handle_response).
    ///
    /// # Arguments
    ///
    /// * `data` - The captured request data including method, URI, headers, and optionally body
    /// * `correlation_id` - A unique identifier for correlating this request with its response
    fn handle_request(&self, data: RequestData, correlation_id: u64);
    /// Handle a captured HTTP response.
    ///
    /// This method is called when a response has been captured by the middleware.
    /// The `correlation_id` matches the one provided to [`handle_request`](Self::handle_request)
    /// for the corresponding request.
    ///
    /// # Arguments
    ///
    /// * `data` - The captured response data including status, headers, body, and timing
    /// * `correlation_id` - The unique identifier that correlates with the original request
    fn handle_response(&self, data: ResponseData, correlation_id: u64);
}

/// Tower layer for the request logging middleware.
///
/// This is the main entry point for using the outlet middleware. It implements the Tower
/// [`Layer`] trait and can be used with Axum's layering system.
///
/// The layer spawns a background task to process captured request/response data using
/// the provided [`RequestHandler`].
///
/// # Examples
///
/// ```rust,no_run
/// use outlet::{RequestLoggerLayer, RequestLoggerConfig, LoggingHandler};
/// use axum::{routing::get, Router};
/// use tower::ServiceBuilder;
///
/// # async fn hello() -> &'static str { "Hello" }
/// # #[tokio::main]
/// # async fn main() {
/// let config = RequestLoggerConfig::default();
/// let handler = LoggingHandler;
/// let layer = RequestLoggerLayer::new(config, handler);
///
/// let app = Router::new()
///     .route("/hello", get(hello))
///     .layer(ServiceBuilder::new().layer(layer));
///
/// let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
/// axum::serve(listener, app).await.unwrap();
/// # }
/// ```
#[derive(Clone)]
pub struct RequestLoggerLayer {
    config: RequestLoggerConfig,
    tx: mpsc::UnboundedSender<BackgroundTask>,
}

impl RequestLoggerLayer {
    /// Create a new request logger layer with the given configuration and handler.
    ///
    /// This spawns a background task that will process captured request/response data
    /// using the provided handler.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration controlling what data to capture
    /// * `handler` - Implementation of [`RequestHandler`] to process the captured data
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use outlet::{RequestLoggerLayer, RequestLoggerConfig, LoggingHandler};
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let config = RequestLoggerConfig {
    ///     capture_request_body: true,
    ///     capture_response_body: true,
    /// };
    /// let handler = LoggingHandler;
    ///
    /// Spawns the background task that runs the provided handler
    /// let layer = RequestLoggerLayer::new(config, handler);
    ///
    /// # use the layer anywhere you'd use a tower layer, and your handler will be called (in the
    /// background) as each request traverses the layer.
    /// # }
    /// ```
    pub fn new<H: RequestHandler>(config: RequestLoggerConfig, handler: H) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<BackgroundTask>();
        let handler = Arc::new(handler);
        let handler_clone = handler.clone();

        // Spawn the background task
        tokio::spawn(async move {
            while let Some(task) = rx.recv().await {
                match task {
                    BackgroundTask::Request(data) => {
                        handler_clone.handle_request(data.clone(), data.correlation_id);
                    }
                    BackgroundTask::Response(data) => {
                        handler_clone.handle_response(data.clone(), data.correlation_id);
                    }
                }
            }
        });

        Self { config, tx }
    }
}

impl<S> Layer<S> for RequestLoggerLayer {
    type Service = RequestLoggerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestLoggerService {
            inner,
            config: self.config.clone(),
            tx: self.tx.clone(),
        }
    }
}

/// Tower service implementation for the request logging middleware.
///
/// This service wraps an inner service and captures request/response data as it flows through.
/// It implements streaming capture, meaning it can handle large request/response bodies without
/// buffering them entirely in memory.
///
/// Users typically don't interact with this type directly - it's created by [`RequestLoggerLayer`].
#[derive(Clone)]
pub struct RequestLoggerService<S> {
    inner: S,
    config: RequestLoggerConfig,
    tx: mpsc::UnboundedSender<BackgroundTask>,
}

impl<S> Service<Request> for RequestLoggerService<S>
where
    S: Service<Request, Response = Response> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[instrument(skip_all)]
    fn call(&mut self, mut request: Request) -> Self::Future {
        let correlation_id = CORRELATION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let start_time = SystemTime::now();

        debug!("Starting request processing");

        // Extract request metadata
        let method = request.method().clone();
        let uri = request.uri().clone();
        let headers = request.headers().clone();

        debug!(method = %method, uri = %uri, "Extracted request metadata");

        let config = self.config.clone();
        let tx = self.tx.clone();
        let tx_clone = tx.clone();

        // Wrap the request body for streaming capture
        if config.capture_request_body {
            debug!(correlation_id = %correlation_id, "Wrapping request body for capture");
            let body = std::mem::replace(request.body_mut(), Body::empty());
            let (body_stream, capture_future) = create_body_capture_stream(body);
            *request.body_mut() = body_stream;
            debug!(correlation_id = %correlation_id, "Request body capture stream created");

            // Spawn background task to collect captured body and send request data
            let tx_clone = tx.clone();
            let method_clone = method.clone();
            let uri_clone = uri.clone();
            let headers_clone = headers.clone();
            tokio::spawn(async move {
                let body = match capture_future.await {
                    Ok(captured_body) => Some(captured_body),
                    Err(e) => {
                        error!(correlation_id = %correlation_id, error = %e, "Error capturing request body");
                        None
                    }
                };

                let request_data = RequestData {
                    correlation_id,
                    timestamp: start_time,
                    method: method_clone,
                    uri: uri_clone,
                    headers: headers_clone,
                    body,
                };

                if tx_clone
                    .send(BackgroundTask::Request(request_data))
                    .is_err()
                {
                    error!(correlation_id = %correlation_id, "Failed to send request data to background task");
                }
            });
        } else {
            let request_data = RequestData {
                correlation_id,
                timestamp: start_time,
                method,
                uri,
                headers,
                body: None,
            };

            if tx.send(BackgroundTask::Request(request_data)).is_err() {
                error!("Failed to send request data to background task");
            }
        }

        debug!("Calling inner service");
        let future = self.inner.call(request);

        Box::pin(async move {
            debug!("Awaiting inner service response");
            let result = future.await;
            debug!("Inner service response received");

            match result {
                Ok(mut response) => {
                    let response_headers = response.headers().clone();
                    let response_status = response.status();
                    let end_time = SystemTime::now();
                    let duration = end_time.duration_since(start_time).unwrap_or_default();

                    if config.capture_response_body {
                        // Wrap the response body for streaming capture
                        let body = std::mem::replace(response.body_mut(), Body::empty());
                        let (body_stream, capture_future) = create_body_capture_stream(body);
                        *response.body_mut() = body_stream;

                        // Spawn task to capture response body
                        tokio::spawn(async move {
                            let body = match capture_future.await {
                                Ok(captured_body) => Some(captured_body),
                                Err(e) => {
                                    error!(error = %e, "Error capturing response body");
                                    None
                                }
                            };

                            let response_data = ResponseData {
                                correlation_id,
                                timestamp: end_time,
                                status: response_status,
                                headers: response_headers,
                                body,
                                duration,
                            };

                            if tx_clone
                                .send(BackgroundTask::Response(response_data))
                                .is_err()
                            {
                                error!("Failed to send response data to background task");
                            }
                        });
                    } else {
                        let response_data = ResponseData {
                            correlation_id,
                            timestamp: end_time,
                            status: response_status,
                            headers: response_headers,
                            body: None,
                            duration,
                        };

                        if tx_clone
                            .send(BackgroundTask::Response(response_data))
                            .is_err()
                        {
                            error!("Failed to send response data to background task");
                        }
                    }

                    Ok(response)
                }
                Err(e) => Err(e),
            }
        })
    }
}

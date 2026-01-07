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
//!     async fn handle_request(&self, data: RequestData) {
//!         // Count requests by endpoint
//!         let endpoint = data.uri.path().to_string();
//!         let mut stats = self.stats.lock().unwrap();
//!         *stats.entry(endpoint).or_insert(0) += 1;
//!     }
//!
//!     async fn handle_response(&self, _request_data: RequestData, response_data: ResponseData) {
//!         // Log slow requests for monitoring
//!         if response_data.duration.as_millis() > 1000 {
//!             println!("SLOW REQUEST: {} took {}ms",
//!                      response_data.status, response_data.duration.as_millis());
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
//!     async fn handle_request(&self, data: RequestData) {
//!         println!("Request: {} {}", data.method, data.uri);
//!         // Custom processing logic here
//!     }
//!
//!     async fn handle_response(&self, _request_data: RequestData, response_data: ResponseData) {
//!         println!("Response: {} ({}ms)", response_data.status, response_data.duration.as_millis());
//!         // Custom processing logic here  
//!     }
//! }
//! ```

use axum::{body::Body, extract::Request, response::Response};
use metrics::counter;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::SystemTime,
};
use tokio::{sync::mpsc, task::JoinSet};
use tower::{Layer, Service};
use tracing::{debug, error, instrument, trace};

pub mod types;
use types::BackgroundTask;
pub use types::{RequestData, ResponseData};

pub mod body_wrapper;
use body_wrapper::create_body_capture_stream;

pub mod logging_handler;
pub use logging_handler::LoggingHandler;

/// Global atomic counter for correlation IDs and process start timestamp
static CORRELATION_COUNTER: AtomicU64 = AtomicU64::new(1);
static PROCESS_START_TIME: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Generate a unique correlation ID combining process start time and counter
fn generate_correlation_id() -> u64 {
    let start_time = *PROCESS_START_TIME.get_or_init(|| {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    });

    let counter = CORRELATION_COUNTER.fetch_add(1, Ordering::Relaxed);

    // High 32 bits: process start timestamp, Low 32 bits: counter
    (start_time << 32) | (counter & 0xFFFFFFFF)
}

/// Convert axum HeaderMap to HashMap<Bytes, Vec<Bytes>>
fn convert_headers(headers: &axum::http::HeaderMap) -> HashMap<String, Vec<bytes::Bytes>> {
    let mut result = HashMap::new();
    for (name, value) in headers {
        let name_bytes = name.as_str().to_owned();
        let value_bytes = bytes::Bytes::copy_from_slice(value.as_bytes());

        result
            .entry(name_bytes)
            .or_insert_with(Vec::new)
            .push(value_bytes);
    }
    result
}

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
///     path_filter: None,
/// };
/// ```
#[derive(Clone, Debug)]
pub struct RequestLoggerConfig {
    /// Whether to capture request bodies
    pub capture_request_body: bool,
    /// Whether to capture response bodies
    pub capture_response_body: bool,
    /// Optional path filter to skip body capture for requests that don't match
    pub path_filter: Option<PathFilter>,
}

/// Filter configuration for determining which requests to capture.
///
/// When configured, the middleware will check this filter BEFORE capturing
/// request/response bodies, avoiding memory overhead for filtered requests.
#[derive(Clone, Debug)]
pub struct PathFilter {
    /// Only capture requests whose URI path starts with any of these prefixes.
    /// If empty, all requests are captured (subject to blocked_prefixes).
    pub allowed_prefixes: Vec<String>,
    /// Skip requests whose URI path starts with any of these prefixes.
    /// Takes precedence over allowed_prefixes.
    pub blocked_prefixes: Vec<String>,
}

impl Default for RequestLoggerConfig {
    fn default() -> Self {
        Self {
            capture_request_body: true,
            capture_response_body: true,
            path_filter: None,
        }
    }
}

impl PathFilter {
    /// Check if a URI path should be captured based on this filter.
    ///
    /// Returns `true` if the request should be captured, `false` if it should be skipped.
    pub fn should_capture(&self, uri: &axum::http::Uri) -> bool {
        let path = uri.path();

        // Check blocked prefixes first (they take precedence)
        for blocked_prefix in &self.blocked_prefixes {
            if path.starts_with(blocked_prefix) {
                return false;
            }
        }

        // If no allowed prefixes specified, allow everything (after blocked check)
        if self.allowed_prefixes.is_empty() {
            return true;
        }

        // Check if URI matches any allowed prefix
        self.allowed_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
    }
}

/// Trait for handling captured request and response data.
///
/// Implement this trait to create custom logic for processing HTTP requests and responses
/// captured by the middleware. The trait provides separate async methods for handling requests
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
///     async fn handle_request(&self, data: RequestData) {
///         info!("Received {} request to {}", data.method, data.uri);
///     }
///
///     async fn handle_response(&self, request_data: RequestData, response_data: ResponseData) {
///         info!("Sent {} response in {}ms",
///               response_data.status, response_data.duration.as_millis());
///     }
/// }
/// ```
pub trait RequestHandler: Send + Sync + 'static {
    /// Handle a captured HTTP request.
    ///
    /// This method is called when a request has been captured by the middleware.
    ///
    /// # Arguments
    ///
    /// * `data` - The captured request data including method, URI, headers, and optionally body
    fn handle_request(&self, data: RequestData) -> impl std::future::Future<Output = ()> + Send;
    /// Handle a captured HTTP response.
    ///
    /// This method is called when a response has been captured by the middleware.
    /// The corresponding request data is provided to allow for correlation and
    /// additional context during response processing.
    ///
    /// # Arguments
    ///
    /// * `request_data` - The corresponding request data for context
    /// * `response_data` - The captured response data including status, headers, body, and timing
    fn handle_response(
        &self,
        request_data: RequestData,
        response_data: ResponseData,
    ) -> impl std::future::Future<Output = ()> + Send;
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
    ///     path_filter: None,
    /// };
    /// let handler = LoggingHandler;
    ///
    /// // Spawns the background task that runs the provided handler
    /// let layer = RequestLoggerLayer::new(config, handler);
    ///
    /// // use the layer anywhere you'd use a tower layer, and your handler will be called (in the
    /// // background) as each request traverses the layer.
    /// # }
    /// ```
    pub fn new<H: RequestHandler>(config: RequestLoggerConfig, handler: H) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<BackgroundTask>();
        let handler = Arc::new(handler);
        let handler_clone = handler.clone();

        // Spawn the background task with concurrent handler execution via JoinSet
        // Unlike FuturesUnordered, JoinSet spawns tasks onto the executor independently,
        // avoiding implicit sequencing that could cause deadlocks with shared resources.
        tokio::spawn(async move {
            let mut join_set = JoinSet::new();

            loop {
                tokio::select! {
                    // Reap completed tasks to prevent unbounded growth
                    Some(result) = join_set.join_next(), if !join_set.is_empty() => {
                        if let Err(e) = result {
                            error!("Handler task panicked: {}", e);
                        }
                    }
                    // Accept new tasks from the channel
                    task = rx.recv() => {
                        match task {
                            Some(task) => {
                                counter!("outlet_queue_dequeued_total").increment(1);
                                let handler = handler_clone.clone();
                                join_set.spawn(async move {
                                    match task {
                                        BackgroundTask::Request(data) => {
                                            handler.handle_request(data).await;
                                        }
                                        BackgroundTask::Response {
                                            request_data,
                                            response_data,
                                        } => {
                                            handler
                                                .handle_response(request_data, response_data)
                                                .await;
                                        }
                                    }
                                });
                            }
                            None => {
                                // Channel closed, wait for remaining handlers to complete
                                while let Some(result) = join_set.join_next().await {
                                    if let Err(e) = result {
                                        error!("Handler task panicked: {}", e);
                                    }
                                }
                                break;
                            }
                        }
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
        // Check filter FIRST - if filtered, pass through immediately with zero overhead
        if let Some(ref filter) = self.config.path_filter {
            if !filter.should_capture(request.uri()) {
                debug!(uri = %request.uri(), "Skipping request due to path filter");
                let future = self.inner.call(request);
                return Box::pin(future);
            }
        }

        // Request passes filter - proceed with full processing
        let correlation_id = generate_correlation_id();
        let start_time = SystemTime::now();

        // Extract request metadata
        let method = request.method().clone();
        let uri = request.uri().clone();
        let headers = request.headers().clone();

        let config = self.config.clone();
        let tx = self.tx.clone();

        let method_clone = method.clone();
        let uri_clone = uri.clone();
        let headers_clone = headers.clone();
        let tx_for_request = tx.clone();
        let tx_for_response = tx.clone();

        trace!(method = %method, uri = %uri, correlation_id = %correlation_id, "Starting request processing");

        // Setup request body capture
        let capture_future = if config.capture_request_body {
            trace!(correlation_id = %correlation_id, "Wrapping request body for capture");
            let body = std::mem::replace(request.body_mut(), Body::empty());
            let (body_stream, capture_future) = create_body_capture_stream(body);
            *request.body_mut() = body_stream;
            trace!(correlation_id = %correlation_id, "Request body capture stream created");
            Some(capture_future)
        } else {
            None
        };

        let request_data_future = tokio::spawn(async move {
            let body = if let Some(capture_future) = capture_future {
                match capture_future.await {
                    Ok(captured_body) => Some(captured_body),
                    Err(e) => {
                        error!(correlation_id = %correlation_id, error = %e, "Error capturing request body");
                        return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
                    }
                }
            } else {
                None
            };

            let request_data = RequestData {
                correlation_id,
                timestamp: start_time,
                method: method_clone,
                uri: uri_clone,
                headers: convert_headers(&headers_clone),
                body,
            };

            if let Err(e) = tx_for_request.send(BackgroundTask::Request(request_data.clone())) {
                error!(correlation_id = %correlation_id, error = %e, "Failed to send request data to background task");
                return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
            }
            counter!("outlet_queue_enqueued_total").increment(1);

            Ok(request_data)
        });

        let future = self.inner.call(request);

        Box::pin(async move {
            trace!("Awaiting inner service response");
            let result = future.await;
            trace!("Inner service response received");

            match result {
                Ok(mut response) => {
                    let response_headers = response.headers().clone();
                    let response_status = response.status();
                    let first_byte_time = SystemTime::now();
                    let duration_to_first_byte = first_byte_time
                        .duration_since(start_time)
                        .unwrap_or_default();

                    // Setup response body capture
                    let capture_future = if config.capture_response_body {
                        trace!(correlation_id = %correlation_id, "Wrapping response body for capture");
                        let body = std::mem::replace(response.body_mut(), Body::empty());
                        let (body_stream, capture_future) = create_body_capture_stream(body);
                        *response.body_mut() = body_stream;
                        trace!(correlation_id = %correlation_id, "Response body capture stream created");
                        Some(capture_future)
                    } else {
                        None
                    };

                    // The future that outlives the request/response lifecycle
                    tokio::spawn(async move {
                        // Await request data future completion first
                        let request_data = match request_data_future.await {
                            Ok(Ok(data)) => data,
                            Ok(Err(e)) => {
                                error!(correlation_id = %correlation_id, error = %e, "Error processing request data");
                                return; // Early return if we can't process request data
                            }
                            Err(e) => {
                                error!(correlation_id = %correlation_id, error = %e, "Error retrieving request data");
                                return; // Early return if we can't get request data
                            }
                        };

                        let (body, total_duration) = if let Some(capture_future) = capture_future {
                            match capture_future.await {
                                Ok(captured_body) => {
                                    let stream_completion_time = SystemTime::now();
                                    let total_duration = stream_completion_time
                                        .duration_since(start_time)
                                        .unwrap_or_default();
                                    (Some(captured_body), total_duration)
                                }
                                Err(e) => {
                                    error!(correlation_id = %correlation_id, error = %e, "Error capturing response body");
                                    (None, duration_to_first_byte)
                                }
                            }
                        } else {
                            // No streaming - total duration equals first byte duration
                            (None, duration_to_first_byte)
                        };

                        let response_data = ResponseData {
                            correlation_id,
                            timestamp: first_byte_time,
                            status: response_status,
                            headers: convert_headers(&response_headers),
                            body,
                            duration_to_first_byte,
                            duration: total_duration,
                        };

                        if tx_for_response
                            .send(BackgroundTask::Response {
                                request_data,
                                response_data,
                            })
                            .is_err()
                        {
                            error!(correlation_id = %correlation_id, "Failed to send response data to background task");
                        } else {
                            counter!("outlet_queue_enqueued_total").increment(1);
                        }
                    });

                    Ok(response)
                }
                Err(e) => Err(e),
            }
        })
    }
}

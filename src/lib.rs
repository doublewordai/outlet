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
use opentelemetry::trace::TraceContextExt;
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
use tokio::sync::mpsc;
use tower::{Layer, Service};
use tracing::{debug, error, trace, Instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub mod types;
use types::BackgroundTask;
pub use types::{RequestData, ResponseData};

pub mod body_wrapper;
use body_wrapper::create_body_capture_stream;

pub mod logging_handler;
pub use logging_handler::LoggingHandler;

pub mod multi_handler;
pub use multi_handler::MultiHandler;

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
///     ..Default::default()
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
    /// Capacity of the bounded channel between the middleware and the background
    /// processing task. When the channel is full, new items are dropped (with a
    /// counter increment on `outlet_queue_dropped_total`) rather than applying
    /// backpressure to the request path. Default: 4096.
    pub channel_capacity: usize,
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
            channel_capacity: 4096,
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
/// Drop-fired sentinel that `try_send`s a [`BackgroundTask::Abandoned`] if
/// not [`disarm`](Self::disarm)ed first.
///
/// Used by [`RequestLoggerService::call`] to detect the "client cancelled
/// before any response was produced" path: the outer future is dropped
/// before reaching the `Ok(response)` arm, so the detached response task
/// is never spawned and `BackgroundTask::Response` never fires. The guard
/// catches that drop and feeds an `Abandoned` event into the same
/// background channel.
///
/// The send is best-effort (`try_send`) because Drop runs synchronously
/// and can't await; if the channel is full or closed, the abandon event is
/// dropped along with the `outlet_queue_dropped_total` counter, same as
/// other tasks under back-pressure.
struct AbandonGuard {
    tx: mpsc::Sender<BackgroundTask>,
    data: Option<RequestData>,
}

impl AbandonGuard {
    fn new(tx: mpsc::Sender<BackgroundTask>, data: RequestData) -> Self {
        Self {
            tx,
            data: Some(data),
        }
    }

    /// Consume the held request data so the guard's Drop becomes a no-op.
    /// Called once the inner future yields a value — at that point the
    /// outer flow has either spawned the response task (Ok) or returned
    /// an error (Err), and neither case is abandonment.
    fn disarm(&mut self) {
        self.data.take();
    }
}

impl Drop for AbandonGuard {
    fn drop(&mut self) {
        if let Some(data) = self.data.take() {
            if self
                .tx
                .try_send(BackgroundTask::Abandoned { data })
                .is_err()
            {
                counter!("outlet_queue_dropped_total").increment(1);
            } else {
                counter!("outlet_queue_enqueued_total").increment(1);
            }
        }
    }
}

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

    /// Handle a request whose handler future was dropped before a response
    /// was produced.
    ///
    /// This fires when the inner service future is dropped before completing
    /// — typically because the client cancelled the connection while an
    /// upstream call was still in flight. The `request_data` carries
    /// whatever was captured synchronously at request time; the request
    /// body is always `None` because body capture is decoupled (it lives
    /// on a separate spawned task) and racing it would slow down the
    /// abandon path's `try_send`.
    ///
    /// Default implementation is a no-op so adding this trait method is not
    /// a breaking change. Override it if you maintain per-request state
    /// that needs cleanup on abandonment (e.g. terminating a row that
    /// `handle_response` would otherwise have finalized).
    ///
    /// # Relationship to `handle_request` and `handle_response`
    ///
    /// `handle_request` runs from a task spawned independently of the
    /// inner service future; it may fire *before* `handle_abandoned`,
    /// *after*, or not at all, depending on whether request body capture
    /// completes before the drop. Handlers must NOT assume `handle_request`
    /// has already been processed when `handle_abandoned` fires, and any
    /// state created in `handle_request` should be cleaned up
    /// idempotently here.
    ///
    /// `handle_response` and `handle_abandoned` are mutually exclusive for
    /// a given request: the same drop guard that fires `Abandoned` is
    /// disarmed the moment the inner future resolves to `Ok(response)` (at
    /// which point a detached task takes over and fires `Response`).
    ///
    /// This hook only fires for the "client cancelled before response
    /// started" path. If the response began streaming and the client then
    /// dropped, `handle_response` fires with whatever body was captured
    /// before the stream broke — outlet's response-task spawn outlives the
    /// outer future once the inner service yields headers.
    fn handle_abandoned(&self, _data: RequestData) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    /// Handle a batch of captured HTTP requests.
    ///
    /// Called by the background task with all requests that have accumulated since
    /// the last flush. The default implementation clones each item and calls
    /// [`handle_request`] concurrently via `join_all`.
    ///
    /// Override this method to perform bulk operations (e.g. batch INSERT)
    /// that can borrow directly from the slice without cloning.
    fn handle_request_batch(
        &self,
        batch: &[RequestData],
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let futures: Vec<_> = batch
                .iter()
                .map(|data| self.handle_request(data.clone()))
                .collect();
            futures::future::join_all(futures).await;
        }
    }

    /// Handle a batch of captured HTTP responses.
    ///
    /// Called by the background task with all responses that have accumulated since
    /// the last flush. The default implementation clones each item and calls
    /// [`handle_response`] concurrently via `join_all`.
    ///
    /// Override this method to perform bulk operations (e.g. batch INSERT)
    /// that can borrow directly from the slice without cloning.
    fn handle_response_batch(
        &self,
        batch: &[(RequestData, ResponseData)],
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let futures: Vec<_> = batch
                .iter()
                .map(|(req, res)| self.handle_response(req.clone(), res.clone()))
                .collect();
            futures::future::join_all(futures).await;
        }
    }

    /// Handle a batch of abandoned requests.
    ///
    /// Default implementation calls [`handle_abandoned`] for each item
    /// concurrently. Override for bulk operations.
    fn handle_abandoned_batch(
        &self,
        batch: &[RequestData],
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let futures: Vec<_> = batch
                .iter()
                .map(|data| self.handle_abandoned(data.clone()))
                .collect();
            futures::future::join_all(futures).await;
        }
    }
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
    tx: mpsc::Sender<BackgroundTask>,
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
    ///     ..Default::default()
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
        let (tx, mut rx) = mpsc::channel::<BackgroundTask>(config.channel_capacity);
        let handler = Arc::new(handler);
        let handler_clone = handler.clone();

        // Spawn the background task using write-through batching.
        // Waits for at least one item, drains all available items, then flushes
        // the batch to the handler. This gives low latency at low load (single
        // item → immediate flush) and batching efficiency at high load.
        tokio::spawn(async move {
            loop {
                // Block until at least one item arrives (or channel closes)
                let first = match rx.recv().await {
                    Some(task) => task,
                    None => break, // channel closed, shut down
                };

                // Non-blocking drain of all queued items
                let mut tasks = vec![first];
                while let Ok(task) = rx.try_recv() {
                    tasks.push(task);
                }

                counter!("outlet_queue_dequeued_total").increment(tasks.len() as u64);

                // Separate into request, response, and abandoned batches
                let mut request_batch = Vec::new();
                let mut response_batch = Vec::new();
                let mut abandoned_batch = Vec::new();

                for task in tasks {
                    match task {
                        BackgroundTask::Request { data } => {
                            request_batch.push(data);
                        }
                        BackgroundTask::Response {
                            request_data,
                            response_data,
                            ..
                        } => {
                            response_batch.push((request_data, response_data));
                        }
                        BackgroundTask::Abandoned { data } => {
                            abandoned_batch.push(data);
                        }
                    }
                }

                // Dispatch batches concurrently. Each batch is spawned as a
                // task so a panic in one handler doesn't kill the loop.
                let handler = handler_clone.clone();
                let req_handle = if !request_batch.is_empty() {
                    let h = handler.clone();
                    Some(tokio::spawn(async move {
                        h.handle_request_batch(&request_batch).await;
                    }))
                } else {
                    None
                };
                let res_handle = if !response_batch.is_empty() {
                    let h = handler.clone();
                    Some(tokio::spawn(async move {
                        let span = tracing::info_span!(
                            "outlet.handle_response_batch",
                            batch_size = response_batch.len(),
                        );
                        h.handle_response_batch(&response_batch)
                            .instrument(span)
                            .await;
                    }))
                } else {
                    None
                };
                let abandoned_handle = if !abandoned_batch.is_empty() {
                    let h = handler.clone();
                    Some(tokio::spawn(async move {
                        let span = tracing::info_span!(
                            "outlet.handle_abandoned_batch",
                            batch_size = abandoned_batch.len(),
                        );
                        h.handle_abandoned_batch(&abandoned_batch)
                            .instrument(span)
                            .await;
                    }))
                } else {
                    None
                };
                if let Some(handle) = req_handle {
                    if let Err(e) = handle.await {
                        error!("Request batch handler panicked: {}", e);
                    }
                }
                if let Some(handle) = res_handle {
                    if let Err(e) = handle.await {
                        error!("Response batch handler panicked: {}", e);
                    }
                }
                if let Some(handle) = abandoned_handle {
                    if let Err(e) = handle.await {
                        error!("Abandoned batch handler panicked: {}", e);
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
    tx: mpsc::Sender<BackgroundTask>,
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

        // Create span with OTel HTTP semantic conventions
        let span = tracing::info_span!(
            "outlet.call",
            http.request.method = %method,
            url.path = %uri.path(),
            url.query = uri.query().unwrap_or(""),
        );
        let headers = request.headers().clone();

        let config = self.config.clone();
        let tx = self.tx.clone();

        let method_clone = method.clone();
        let uri_clone = uri.clone();
        let headers_clone = headers.clone();
        let tx_for_request = tx.clone();
        let tx_for_response = tx.clone();

        // Capture trace_id and span_id from current span for RequestData
        let (trace_id, span_id) = {
            let ctx = tracing::Span::current().context();
            let span_ref = ctx.span();
            let span_ctx = span_ref.span_context();
            if span_ctx.is_valid() {
                (
                    Some(span_ctx.trace_id().to_string()),
                    Some(span_ctx.span_id().to_string()),
                )
            } else {
                (None, None)
            }
        };
        // Cloned copies for the abandon guard built below — the originals are
        // moved into the request_data_future spawn.
        let trace_id_for_abandon = trace_id.clone();
        let span_id_for_abandon = span_id.clone();

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
                trace_id: trace_id.clone(),
                span_id: span_id.clone(),
            };

            if let Err(e) = tx_for_request.try_send(BackgroundTask::Request {
                data: request_data.clone(),
            }) {
                counter!("outlet_queue_dropped_total").increment(1);
                error!(correlation_id = %correlation_id, error = %e, "Dropped request data: channel full or closed");
                return Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
            }
            counter!("outlet_queue_enqueued_total").increment(1);

            Ok(request_data)
        });

        let future = self.inner.call(request);

        // Drop guard for the "client cancelled before any response" path.
        // If the outer future is dropped between here and the `Ok(response)`
        // arm (which spawns a detached task that owns continuation), the
        // guard's Drop synchronously try_sends a `BackgroundTask::Abandoned`
        // so handlers can clean up per-request state. We build a minimal
        // RequestData with no body — the real body capture races the inner
        // drop and isn't guaranteed to land — and feed it to the guard.
        // After we reach `Ok(response)`, the guard is disarmed so normal
        // completion doesn't double-fire alongside `BackgroundTask::Response`.
        let abandon_data = RequestData {
            correlation_id,
            timestamp: start_time,
            method,
            uri,
            headers: convert_headers(&headers),
            body: None,
            trace_id: trace_id_for_abandon,
            span_id: span_id_for_abandon,
        };
        let mut abandon_guard = AbandonGuard::new(self.tx.clone(), abandon_data);

        Box::pin(async move {
            trace!("Awaiting inner service response");
            let result = future.await;
            trace!("Inner service response received");

            // Inner future resolved (Ok or Err) — not an abandonment. Disarm
            // before the match so neither arm double-fires alongside the
            // detached response task or with an Err return.
            abandon_guard.disarm();

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
                            .try_send(BackgroundTask::Response {
                                request_data,
                                response_data,
                            })
                            .is_err()
                        {
                            counter!("outlet_queue_dropped_total").increment(1);
                            error!(correlation_id = %correlation_id, "Dropped response data: channel full or closed");
                        } else {
                            counter!("outlet_queue_enqueued_total").increment(1);
                        }
                    });

                    Ok(response)
                }
                Err(e) => Err(e),
            }
        }.instrument(span))
    }
}

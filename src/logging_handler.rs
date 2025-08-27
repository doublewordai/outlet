//! Simple logging implementation for demonstration purposes.
//!
//! This module provides a basic [`LoggingHandler`] that logs captured
//! requests and responses using the `tracing` crate. It serves as both
//! a functional handler and an example of how to implement [`RequestHandler`].

use tracing::info;

use crate::{RequestData, RequestHandler, ResponseData};

/// Simple logging implementation of [`RequestHandler`].
///
/// This handler logs captured requests and responses using structured logging
/// via the `tracing` crate. It's primarily intended for development, debugging,
/// and as an example implementation.
///
/// The logged information includes:
/// - Correlation ID for matching requests with responses
/// - HTTP method and URI for requests
/// - Status code and duration for responses
/// - Headers (in debug format)
/// - Body size (if body capture is enabled)
///
/// # Examples
///
/// ```rust,no_run
/// use outlet::{RequestLoggerLayer, RequestLoggerConfig, LoggingHandler};
///
/// # #[tokio::main]
/// # async fn main() {
/// let config = RequestLoggerConfig::default();
/// let handler = LoggingHandler;
/// let layer = RequestLoggerLayer::new(config, handler);
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct LoggingHandler;

impl RequestHandler for LoggingHandler {
    fn handle_request(&self, data: RequestData, correlation_id: u64) {
        info!(
            correlation_id = %correlation_id,
            method = %data.method,
            uri = %data.uri,
            headers = ?data.headers,
            body_size = data.body.as_ref().map(|b| b.len()).unwrap_or(0),
            "Request captured"
        );
    }

    fn handle_response(&self, data: ResponseData, correlation_id: u64) {
        info!(
            correlation_id = %correlation_id,
            status = %data.status,
            headers = ?data.headers,
            body_size = data.body.as_ref().map(|b| b.len()).unwrap_or(0),
            duration_ms = data.duration.as_millis(),
            "Response captured"
        );
    }
}
//! Data types for captured HTTP request and response information.
//!
//! This module contains the core data structures used to represent captured
//! HTTP requests and responses, along with background task types.

use axum::http::{Method, StatusCode, Uri};
use bytes::Bytes;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// Data captured from an HTTP request.
///
/// Contains all the information about an HTTP request that was captured by the middleware.
/// The body is optionally captured based on the configuration.
///
/// # Examples
///
/// ```rust
/// use outlet::types::RequestData;
/// use axum::http::{Method, Uri};
/// use std::time::SystemTime;
///
/// // This struct is typically created by the middleware, but you might
/// // encounter it in your RequestHandler implementation
/// ```
#[derive(Debug, Clone)]
pub struct RequestData {
    /// Unique identifier for correlating this request with its response
    pub correlation_id: u64,
    /// When the request was received by the middleware
    pub timestamp: SystemTime,
    /// HTTP method (GET, POST, etc.)
    pub method: Method,
    /// Request URI including path and query parameters
    pub uri: Uri,
    /// HTTP request headers as key-value pairs (values as raw bytes - no encoding guarantees)
    pub headers: HashMap<String, Vec<Bytes>>,
    /// Request body bytes, if body capture is enabled and the body was successfully captured
    pub body: Option<Bytes>,
}

/// Data captured from an HTTP response.
///
/// Contains all the information about an HTTP response that was captured by the middleware.
/// Includes timing information and optionally the response body.
///
/// # Examples
///
/// ```rust
/// use outlet::types::ResponseData;
/// use axum::http::StatusCode;
/// use std::time::{Duration, SystemTime};
///
/// // This struct is typically created by the middleware, but you might
/// // encounter it in your RequestHandler implementation
/// ```
#[derive(Debug, Clone)]
pub struct ResponseData {
    /// Correlation ID matching the original request
    pub correlation_id: u64,
    /// When the response was sent by the middleware
    pub timestamp: SystemTime,
    /// HTTP status code (200, 404, 500, etc.)
    pub status: StatusCode,
    /// HTTP request headers as key-value pairs (values as raw bytes - no encoding guarantees)
    pub headers: HashMap<String, Vec<Bytes>>,
    /// Response body bytes, if body capture is enabled and the body was successfully captured
    pub body: Option<Bytes>,
    /// Time elapsed from when the request was received to when response headers were ready
    pub duration_to_first_byte: Duration,
    /// Total time elapsed from request start to completion of response stream.
    /// For non-streaming responses, this equals duration_to_first_byte.
    /// For streaming responses, this is the time until the stream fully completes.
    pub duration: Duration,
}

/// Tasks sent to the background processing task.
///
/// The middleware sends these tasks to a background async task for processing.
/// This allows the main request/response flow to continue without blocking.
///
/// Users typically don't interact with this type directly.
#[derive(Debug)]
pub(crate) enum BackgroundTask {
    /// A request has been captured and is ready for processing
    Request { data: RequestData },
    /// A response has been captured and is ready for processing.
    /// Contains both the request and response data to provide full context.
    Response {
        request_data: RequestData,
        response_data: ResponseData,
        /// OpenTelemetry trace ID from the original request, for creating span links
        trace_id: Option<String>,
    },
}

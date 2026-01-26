//! Composite handler for combining multiple request handlers.
//!
//! This module provides [`MultiHandler`], a flexible handler that can compose an arbitrary number
//! of [`RequestHandler`] implementations. All handlers run concurrently for both request and
//! response phases.
//!
//! # Example
//!
//! ```rust
//! use outlet::{MultiHandler, RequestHandler, RequestData, ResponseData, LoggingHandler};
//!
//! // Create a composite handler
//! let multi_handler = MultiHandler::new()
//!     .with(LoggingHandler);
//!
//! // Use with RequestLoggerLayer
//! // let layer = RequestLoggerLayer::new(config, multi_handler);
//! ```

use crate::{RequestData, RequestHandler, ResponseData};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Type alias for boxed futures used in the dyn-compatible wrapper.
type BoxFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Internal trait that is dyn-compatible for type erasure.
/// This wraps RequestHandler implementations to allow storing them as trait objects.
trait DynHandler: Send + Sync + 'static {
    fn handle_request_boxed(&self, data: RequestData) -> BoxFuture<'_>;
    fn handle_response_boxed(&self, request_data: RequestData, response_data: ResponseData) -> BoxFuture<'_>;
}

/// Wrapper that implements DynHandler for any RequestHandler.
struct HandlerWrapper<H: RequestHandler> {
    inner: H,
}

impl<H: RequestHandler> DynHandler for HandlerWrapper<H> {
    fn handle_request_boxed(&self, data: RequestData) -> BoxFuture<'_> {
        Box::pin(self.inner.handle_request(data))
    }

    fn handle_response_boxed(&self, request_data: RequestData, response_data: ResponseData) -> BoxFuture<'_> {
        Box::pin(self.inner.handle_response(request_data, response_data))
    }
}

/// A handler that delegates to multiple inner handlers.
///
/// Handlers are executed concurrently for both `handle_request` and `handle_response`.
/// This allows you to compose analytics, logging, and other handlers together.
///
/// # Thread Safety
///
/// `MultiHandler` is `Send + Sync` and can be safely shared across threads.
/// Each inner handler is wrapped in an `Arc` for efficient cloning.
pub struct MultiHandler {
    handlers: Vec<Arc<dyn DynHandler>>,
}

impl MultiHandler {
    /// Create a new empty MultiHandler.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Add a handler to the chain. Returns self for builder pattern.
    ///
    /// # Example
    ///
    /// ```rust
    /// use outlet::{MultiHandler, LoggingHandler};
    ///
    /// let handler = MultiHandler::new()
    ///     .with(LoggingHandler);
    /// ```
    pub fn with<H: RequestHandler>(mut self, handler: H) -> Self {
        self.handlers.push(Arc::new(HandlerWrapper { inner: handler }));
        self
    }

    /// Returns true if no handlers have been added.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Returns the number of handlers in the chain.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }
}

impl Default for MultiHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestHandler for MultiHandler {
    async fn handle_request(&self, data: RequestData) {
        let futures: Vec<_> = self
            .handlers
            .iter()
            .map(|h| {
                let data = data.clone();
                let handler = h.clone();
                async move { handler.handle_request_boxed(data).await }
            })
            .collect();
        futures::future::join_all(futures).await;
    }

    async fn handle_response(&self, request_data: RequestData, response_data: ResponseData) {
        let futures: Vec<_> = self
            .handlers
            .iter()
            .map(|h| {
                let req = request_data.clone();
                let res = response_data.clone();
                let handler = h.clone();
                async move { handler.handle_response_boxed(req, res).await }
            })
            .collect();
        futures::future::join_all(futures).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Method, StatusCode, Uri};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    /// Test handler that counts calls
    struct CountingHandler {
        request_count: Arc<AtomicUsize>,
        response_count: Arc<AtomicUsize>,
    }

    impl RequestHandler for CountingHandler {
        async fn handle_request(&self, _data: RequestData) {
            self.request_count.fetch_add(1, Ordering::SeqCst);
        }

        async fn handle_response(&self, _request_data: RequestData, _response_data: ResponseData) {
            self.response_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn create_test_request_data() -> RequestData {
        RequestData {
            correlation_id: 123,
            timestamp: SystemTime::now(),
            method: Method::GET,
            uri: Uri::from_static("/test"),
            headers: std::collections::HashMap::new(),
            body: None,
        }
    }

    fn create_test_response_data() -> ResponseData {
        ResponseData {
            correlation_id: 123,
            timestamp: SystemTime::now(),
            status: StatusCode::OK,
            headers: std::collections::HashMap::new(),
            body: None,
            duration_to_first_byte: Duration::from_millis(10),
            duration: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn test_multi_handler_empty() {
        let handler = MultiHandler::new();
        assert!(handler.is_empty());
        assert_eq!(handler.len(), 0);

        // Should not panic with no handlers
        handler.handle_request(create_test_request_data()).await;
        handler
            .handle_response(create_test_request_data(), create_test_response_data())
            .await;
    }

    #[tokio::test]
    async fn test_multi_handler_single() {
        let request_count = Arc::new(AtomicUsize::new(0));
        let response_count = Arc::new(AtomicUsize::new(0));

        let handler = MultiHandler::new().with(CountingHandler {
            request_count: request_count.clone(),
            response_count: response_count.clone(),
        });

        assert!(!handler.is_empty());
        assert_eq!(handler.len(), 1);

        handler.handle_request(create_test_request_data()).await;
        assert_eq!(request_count.load(Ordering::SeqCst), 1);

        handler
            .handle_response(create_test_request_data(), create_test_response_data())
            .await;
        assert_eq!(response_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_multi_handler_multiple() {
        let request_count1 = Arc::new(AtomicUsize::new(0));
        let response_count1 = Arc::new(AtomicUsize::new(0));
        let request_count2 = Arc::new(AtomicUsize::new(0));
        let response_count2 = Arc::new(AtomicUsize::new(0));

        let handler = MultiHandler::new()
            .with(CountingHandler {
                request_count: request_count1.clone(),
                response_count: response_count1.clone(),
            })
            .with(CountingHandler {
                request_count: request_count2.clone(),
                response_count: response_count2.clone(),
            });

        assert_eq!(handler.len(), 2);

        handler.handle_request(create_test_request_data()).await;
        assert_eq!(request_count1.load(Ordering::SeqCst), 1);
        assert_eq!(request_count2.load(Ordering::SeqCst), 1);

        handler
            .handle_response(create_test_request_data(), create_test_response_data())
            .await;
        assert_eq!(response_count1.load(Ordering::SeqCst), 1);
        assert_eq!(response_count2.load(Ordering::SeqCst), 1);
    }

    /// Handler that captures the data it receives for verification
    struct CapturingHandler {
        captured_correlation_id: Arc<std::sync::Mutex<Option<u64>>>,
        captured_status: Arc<std::sync::Mutex<Option<StatusCode>>>,
    }

    impl RequestHandler for CapturingHandler {
        async fn handle_request(&self, data: RequestData) {
            *self.captured_correlation_id.lock().unwrap() = Some(data.correlation_id);
        }

        async fn handle_response(&self, _request_data: RequestData, response_data: ResponseData) {
            *self.captured_status.lock().unwrap() = Some(response_data.status);
        }
    }

    #[tokio::test]
    async fn test_handlers_receive_correct_data() {
        let captured_id = Arc::new(std::sync::Mutex::new(None));
        let captured_status = Arc::new(std::sync::Mutex::new(None));

        let handler = MultiHandler::new().with(CapturingHandler {
            captured_correlation_id: captured_id.clone(),
            captured_status: captured_status.clone(),
        });

        // Use specific values we can verify
        let mut request_data = create_test_request_data();
        request_data.correlation_id = 42;

        let mut response_data = create_test_response_data();
        response_data.status = StatusCode::CREATED;

        handler.handle_request(request_data.clone()).await;
        assert_eq!(*captured_id.lock().unwrap(), Some(42));

        handler.handle_response(request_data, response_data).await;
        assert_eq!(*captured_status.lock().unwrap(), Some(StatusCode::CREATED));
    }

    /// Handler that waits at a barrier - proves concurrent execution
    struct BarrierHandler {
        barrier: Arc<tokio::sync::Barrier>,
        completed: Arc<AtomicUsize>,
    }

    impl RequestHandler for BarrierHandler {
        async fn handle_request(&self, _data: RequestData) {
            self.barrier.wait().await;
            self.completed.fetch_add(1, Ordering::SeqCst);
        }

        async fn handle_response(&self, _request_data: RequestData, _response_data: ResponseData) {
            self.barrier.wait().await;
            self.completed.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn test_handlers_run_concurrently() {
        // Barrier requires 2 waiters before any can proceed
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let completed = Arc::new(AtomicUsize::new(0));

        let handler = MultiHandler::new()
            .with(BarrierHandler {
                barrier: barrier.clone(),
                completed: completed.clone(),
            })
            .with(BarrierHandler {
                barrier: barrier.clone(),
                completed: completed.clone(),
            });

        // If sequential: deadlock at barrier. If concurrent: both proceed.
        // Timeout ensures test fails fast if deadlocked.
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            handler.handle_request(create_test_request_data()),
        )
        .await;

        assert!(
            result.is_ok(),
            "Handlers must run concurrently - barrier would deadlock if sequential"
        );
        assert_eq!(completed.load(Ordering::SeqCst), 2);

        // Test response path
        completed.store(0, Ordering::SeqCst);
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            handler.handle_response(create_test_request_data(), create_test_response_data()),
        )
        .await;

        assert!(
            result.is_ok(),
            "Handlers must run concurrently - barrier would deadlock if sequential"
        );
        assert_eq!(completed.load(Ordering::SeqCst), 2);
    }
}

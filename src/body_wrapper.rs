//! Body streaming and capture utilities.
//!
//! This module provides functionality for capturing HTTP request and response bodies
//! while allowing them to continue streaming to their destination.

use axum::body::Bytes;
use futures::{Future, StreamExt};
use http_body_util::BodyExt;
use std::pin::Pin;
use tokio::sync::mpsc;
use tracing::error;

/// Error type for body capture operations
#[derive(Debug, thiserror::Error)]
pub enum BodyCaptureError {
    #[error("Body stream error: {0}")]
    StreamError(String),
}

type CapturedBody = Pin<Box<dyn Future<Output = Result<Bytes, BodyCaptureError>> + Send>>;

/// Creates a body capture stream that captures chunks as they flow through without blocking.
///
/// This function takes an HTTP body and returns a new body stream along with a future
/// that resolves to the captured body content. The returned body stream can be used
/// normally while the capture future collects all the chunks in the background.
///
/// # Arguments
///
/// * `body` - The original HTTP body to capture
///
/// # Returns
///
/// A tuple containing:
/// - A new body stream that passes through all data
/// - A future that resolves to the captured body bytes
///
/// # Examples
///
/// ```rust
/// use outlet::body_wrapper::create_body_capture_stream;
/// use axum::body::Body;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let original_body = Body::from("Hello, World!");
/// let (new_body, capture_future) = create_body_capture_stream(original_body);
///
/// // Use new_body normally in your response
/// // capture_future will resolve to the captured bytes
/// let captured = capture_future.await?;
/// # Ok(())
/// # }
/// ```
pub fn create_body_capture_stream<B>(body: B) -> (axum::body::Body, CapturedBody)
where
    B: axum::body::HttpBody<Data = Bytes, Error = axum::Error> + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Turn the body into a stream, and relay the chunks to the channel
    let capture_stream = body.into_data_stream().map(move |result| {
        let result_for_channel = match &result {
            Ok(chunk) => Ok(chunk.clone()),
            Err(e) => {
                error!(error = %e, "Stream error during body capture");
                Err(BodyCaptureError::StreamError(e.to_string()))
            }
        };
        let _ = tx.send(result_for_channel);
        result // pass through original
    });

    let new_body = axum::body::Body::from_stream(capture_stream);

    // This future consumes and concatenates all chunks received via the channel. Resolves when the
    // stream is finished, or errors.
    let capture_future = Box::pin(async move {
        let mut chunks = Vec::new();
        while let Some(chunk_result) = rx.recv().await {
            match chunk_result {
                Ok(chunk) => chunks.push(chunk),
                Err(e) => return Err(e),
            }
        }
        Ok(chunks.into_iter().fold(Bytes::new(), |mut acc, chunk| {
            acc = [acc.as_ref(), chunk.as_ref()].concat().into();
            acc
        }))
    });

    (new_body, capture_future)
}

#[cfg(test)]
mod tests {
    use super::create_body_capture_stream;
    use axum::body::Body;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn test_streaming_body_capture() {
        let body = Body::from("Hello, World!");

        let (new_body, capture_future) = create_body_capture_stream(body);

        // Start collecting both the new body and captured chunks concurrently
        let collect_task = tokio::spawn(async move {
            let collected = new_body.collect().await.unwrap();
            collected.to_bytes()
        });

        let capture_task = tokio::spawn(async move { capture_future.await.unwrap() });

        let (body_content, captured_content) = tokio::join!(collect_task, capture_task);
        let body_content = body_content.unwrap();
        let captured_content = captured_content.unwrap();

        assert_eq!(body_content, "Hello, World!");
        assert_eq!(captured_content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_basic_streaming() {
        let large_data = "x".repeat(100);
        let body = Body::from(large_data.clone());

        let (new_body, capture_future) = create_body_capture_stream(body);

        // Start collecting both concurrently
        let collect_task = tokio::spawn(async move {
            let collected = new_body.collect().await.unwrap();
            collected.to_bytes()
        });

        let capture_task = tokio::spawn(async move { capture_future.await.unwrap() });

        let (body_result, capture_result) = tokio::join!(collect_task, capture_task);
        let body_content = body_result.unwrap();
        let captured_content = capture_result.unwrap();

        // Full content should flow through
        assert_eq!(body_content, large_data);
        // Capture should match original content
        assert_eq!(captured_content, large_data);
    }
}

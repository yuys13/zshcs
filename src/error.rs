use thiserror::Error;

use crate::document::DocumentError;

/// Type alias for Results returned across `zshcs`.
pub type ZshcsResult<T> = Result<T, ZshcsError>;

/// Comprehensive, type-safe error enum for all failures occurring within `zshcs`.
#[derive(Debug, Error)]
pub enum ZshcsError {
    /// Document management and synchronization errors.
    #[error("document error: {0}")]
    Document(#[from] DocumentError),

    /// Standard I/O errors occurring during file operations, pipe communications, or process execution.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Completion daemon errors or failures returned by capture scripts.
    #[error("completion daemon error: {0}")]
    Daemon(String),

    /// Channel communication errors when dispatching requests to the daemon.
    #[error("failed to send request to completion daemon: {0}")]
    DaemonChannel(String),

    /// Oneshot receiver dropped or cancelled prior to completion response.
    #[error("completion request cancelled or responder dropped: {0}")]
    RequestCancelled(#[from] tokio::sync::oneshot::error::RecvError),

    /// Operation or request timeout.
    #[error("operation timed out: {0}")]
    Timeout(#[from] tokio::time::error::Elapsed),

    /// Serialization or deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Server or script deployment initialization error.
    #[error("initialization error: {0}")]
    Initialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use tower_lsp::lsp_types::{Position, Range, Url};

    #[test]
    fn test_document_error_conversion_and_display() {
        let uri = Url::parse("file:///path/to/test.zsh").unwrap();
        let doc_err = DocumentError::NotFound(uri);
        let err: ZshcsError = doc_err.into();

        assert!(matches!(
            err,
            ZshcsError::Document(DocumentError::NotFound(_))
        ));
        assert_eq!(
            err.to_string(),
            "document error: document not found: file:///path/to/test.zsh"
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn test_invalid_range_document_error_conversion() {
        let range = Range::new(Position::new(0, 10), Position::new(0, 2));
        let doc_err = DocumentError::InvalidRange(range);
        let err: ZshcsError = doc_err.into();

        assert_eq!(
            err.to_string(),
            format!("document error: invalid range {range:?}")
        );
    }

    #[test]
    fn test_outdated_version_document_error_conversion() {
        let doc_err = DocumentError::OutdatedVersion {
            current: 5,
            received: 3,
        };
        let err: ZshcsError = doc_err.into();

        assert_eq!(
            err.to_string(),
            "document error: outdated version received: current 5, received 3"
        );
    }

    #[test]
    fn test_io_error_conversion_and_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: ZshcsError = io_err.into();

        assert!(matches!(err, ZshcsError::Io(_)));
        assert_eq!(err.to_string(), "I/O error: file not found");
        assert!(err.source().is_some());
    }

    #[test]
    fn test_daemon_error_display() {
        let err = ZshcsError::Daemon("zpty module not found".to_string());
        assert!(matches!(err, ZshcsError::Daemon(_)));
        assert_eq!(
            err.to_string(),
            "completion daemon error: zpty module not found"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn test_daemon_channel_error_display() {
        let err = ZshcsError::DaemonChannel("channel closed".to_string());
        assert!(matches!(err, ZshcsError::DaemonChannel(_)));
        assert_eq!(
            err.to_string(),
            "failed to send request to completion daemon: channel closed"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn test_request_cancelled_error_conversion_and_display() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        drop(tx);
        let recv_err = rx.blocking_recv().unwrap_err();
        let err: ZshcsError = recv_err.into();

        assert!(matches!(err, ZshcsError::RequestCancelled(_)));
        assert_eq!(
            err.to_string(),
            "completion request cancelled or responder dropped: channel closed"
        );
        assert!(err.source().is_some());
    }

    #[tokio::test]
    async fn test_timeout_error_conversion_and_display() {
        let result = tokio::time::timeout(std::time::Duration::from_millis(1), async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        })
        .await;

        let elapsed_err = result.unwrap_err();
        let err: ZshcsError = elapsed_err.into();

        assert!(matches!(err, ZshcsError::Timeout(_)));
        assert_eq!(err.to_string(), "operation timed out: deadline has elapsed");
        assert!(err.source().is_some());
    }

    #[test]
    fn test_serialization_error_conversion_and_display() {
        let json_err = serde_json::from_str::<serde_json::Value>("{invalid_json").unwrap_err();
        let err: ZshcsError = json_err.into();

        assert!(matches!(err, ZshcsError::Serialization(_)));
        assert!(err.to_string().starts_with("serialization error: "));
        assert!(err.source().is_some());
    }

    #[test]
    fn test_initialization_error_display() {
        let err = ZshcsError::Initialization("permission denied on tempdir".to_string());
        assert!(matches!(err, ZshcsError::Initialization(_)));
        assert_eq!(
            err.to_string(),
            "initialization error: permission denied on tempdir"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn test_zshcs_result_try_operator() {
        fn succeed() -> ZshcsResult<i32> {
            Ok(42)
        }

        fn fail_io() -> ZshcsResult<i32> {
            let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
            Err(io_err)?
        }

        fn fail_doc() -> ZshcsResult<i32> {
            let doc_err = DocumentError::NotFound(Url::parse("file:///a.zsh").unwrap());
            Err(doc_err)?
        }

        assert_eq!(succeed().unwrap(), 42);
        assert!(matches!(fail_io(), Err(ZshcsError::Io(_))));
        assert!(matches!(fail_doc(), Err(ZshcsError::Document(_))));
    }

    #[test]
    fn test_error_debug_formatting() {
        let err = ZshcsError::Daemon("test debug".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("Daemon(\"test debug\")"));
    }

    #[test]
    fn test_error_sources_completeness() {
        let uri = Url::parse("file:///b.zsh").unwrap();
        let doc_err: ZshcsError = DocumentError::NotFound(uri).into();
        assert!(doc_err.source().is_some());

        let io_err: ZshcsError = std::io::Error::other("io failure").into();
        assert!(io_err.source().is_some());

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        drop(tx);
        let req_err: ZshcsError = rx.blocking_recv().unwrap_err().into();
        assert!(req_err.source().is_some());
    }
}

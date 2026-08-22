//! Structured logging configuration and initialization for `zshcs`.
//!
//! Provides structured logging to `stderr` using `tracing` and `tracing-subscriber`.
//! The log filter level is dynamically configured via the `ZSHCS_LOG` or `RUST_LOG`
//! environment variables, defaulting to `info`.
//!
//! Because `zshcs` uses `stdio` for LSP JSON-RPC communication, logging MUST NEVER
//! write to `stdout`. All logs are strictly routed to `stderr`.

use tracing_subscriber::EnvFilter;

/// Creates an [`EnvFilter`] based on environment variables.
///
/// Priority:
/// 1. `ZSHCS_LOG` environment variable
/// 2. `RUST_LOG` environment variable
/// 3. Default fallback: `"info"`
pub fn create_env_filter() -> EnvFilter {
    if let Ok(val) = std::env::var("ZSHCS_LOG")
        && !val.trim().is_empty()
        && let Ok(filter) = EnvFilter::try_new(&val)
    {
        return filter;
    }

    if let Ok(val) = std::env::var("RUST_LOG")
        && !val.trim().is_empty()
        && let Ok(filter) = EnvFilter::try_new(&val)
    {
        return filter;
    }

    EnvFilter::new("info")
}

/// Initializes the global tracing subscriber with `stderr` writer.
///
/// If a global subscriber has already been initialized (e.g. in test suites),
/// this function silently ignores the initialization error.
pub fn init_logging() {
    let _ = try_init_logging();
}

/// Attempts to initialize the global tracing subscriber with `stderr` writer.
///
/// Returns an error if the subscriber could not be registered (e.g., if a global
/// default subscriber has already been set).
pub fn try_init_logging() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = create_env_filter();
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[derive(Clone)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for SharedBuffer {
        type Writer = BufferWriter;

        fn make_writer(&'a self) -> Self::Writer {
            BufferWriter(self.0.clone())
        }
    }

    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_create_env_filter_default() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("ZSHCS_LOG");
            std::env::remove_var("RUST_LOG");
        }
        let filter = create_env_filter();
        let filter_str = filter.to_string();
        assert_eq!(filter_str, "info");
    }

    #[test]
    fn test_create_env_filter_with_zshcs_log() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("RUST_LOG");
            std::env::set_var("ZSHCS_LOG", "debug");
        }
        let filter = create_env_filter();
        let filter_str = filter.to_string();
        assert_eq!(filter_str, "debug");
        unsafe {
            std::env::remove_var("ZSHCS_LOG");
        }
    }

    #[test]
    fn test_create_env_filter_with_rust_log() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("ZSHCS_LOG");
            std::env::set_var("RUST_LOG", "warn");
        }
        let filter = create_env_filter();
        let filter_str = filter.to_string();
        assert_eq!(filter_str, "warn");
        unsafe {
            std::env::remove_var("RUST_LOG");
        }
    }

    #[test]
    fn test_create_env_filter_priority() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("ZSHCS_LOG", "trace");
            std::env::set_var("RUST_LOG", "error");
        }
        let filter = create_env_filter();
        let filter_str = filter.to_string();
        assert_eq!(filter_str, "trace");
        unsafe {
            std::env::remove_var("ZSHCS_LOG");
            std::env::remove_var("RUST_LOG");
        }
    }

    #[test]
    fn test_create_env_filter_invalid_zshcs_log_falls_back_to_rust_log() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("ZSHCS_LOG", "invalid[filter");
            std::env::set_var("RUST_LOG", "warn");
        }
        let filter = create_env_filter();
        let filter_str = filter.to_string();
        assert_eq!(filter_str, "warn");
        unsafe {
            std::env::remove_var("ZSHCS_LOG");
            std::env::remove_var("RUST_LOG");
        }
    }

    #[test]
    fn test_create_env_filter_both_invalid_falls_back_to_default() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("ZSHCS_LOG", "invalid[1");
            std::env::set_var("RUST_LOG", "invalid[2");
        }
        let filter = create_env_filter();
        let filter_str = filter.to_string();
        assert_eq!(filter_str, "info");
        unsafe {
            std::env::remove_var("ZSHCS_LOG");
            std::env::remove_var("RUST_LOG");
        }
    }

    #[test]
    fn test_create_env_filter_empty_string_falls_back() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("ZSHCS_LOG", "");
            std::env::set_var("RUST_LOG", "error");
        }
        let filter = create_env_filter();
        let filter_str = filter.to_string();
        assert_eq!(filter_str, "error");
        unsafe {
            std::env::remove_var("ZSHCS_LOG");
            std::env::remove_var("RUST_LOG");
        }
    }

    #[test]
    fn test_init_logging_idempotent() {
        // Calling init_logging multiple times should not panic
        init_logging();
        init_logging();
    }

    #[test]
    fn test_logging_subscriber_output_formatting() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let shared = SharedBuffer(buffer.clone());

        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("trace"))
            .with_writer(shared)
            .with_ansi(false)
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(event = "test_event", key = "value", "Structured message");
            tracing::debug!("Debug message");
            tracing::warn!("Warning message");
            tracing::error!("Error message");
            tracing::trace!("Trace message");
        });

        let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert!(output.contains("INFO"));
        assert!(output.contains("Structured message"));
        assert!(output.contains("test_event"));
        assert!(output.contains("DEBUG"));
        assert!(output.contains("Debug message"));
        assert!(output.contains("WARN"));
        assert!(output.contains("Warning message"));
        assert!(output.contains("ERROR"));
        assert!(output.contains("Error message"));
        assert!(output.contains("TRACE"));
        assert!(output.contains("Trace message"));
    }
}

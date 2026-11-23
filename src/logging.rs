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
    let zshcs_log = std::env::var("ZSHCS_LOG").ok();
    let rust_log = std::env::var("RUST_LOG").ok();
    create_env_filter_from(zshcs_log.as_deref(), rust_log.as_deref())
}

pub(crate) fn create_env_filter_from(zshcs_log: Option<&str>, rust_log: Option<&str>) -> EnvFilter {
    if let Some(val) = zshcs_log
        && !val.trim().is_empty()
        && let Ok(filter) = EnvFilter::try_new(val.trim())
    {
        return filter;
    }

    if let Some(val) = rust_log
        && !val.trim().is_empty()
        && let Ok(filter) = EnvFilter::try_new(val.trim())
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
    use rstest::rstest;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set<V: AsRef<std::ffi::OsStr>>(key: &'static str, value: V) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: Tests calling this hold ENV_MUTEX.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: Tests calling this hold ENV_MUTEX.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: Tests calling this hold ENV_MUTEX.
            unsafe {
                match &self.original {
                    Some(val) => std::env::set_var(self.key, val),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

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

    #[rstest]
    #[case(None, None, "info")]
    #[case(Some("debug"), None, "debug")]
    #[case(None, Some("warn"), "warn")]
    #[case(Some("trace"), Some("error"), "trace")]
    #[case(Some("invalid[filter"), Some("warn"), "warn")]
    #[case(Some("invalid[1"), Some("invalid[2"), "info")]
    #[case(Some(""), Some("error"), "error")]
    #[case(Some("  debug  "), None, "debug")]
    fn test_create_env_filter_from_cases(
        #[case] zshcs_log: Option<&str>,
        #[case] rust_log: Option<&str>,
        #[case] expected: &str,
    ) {
        let filter = create_env_filter_from(zshcs_log, rust_log);
        assert_eq!(filter.to_string(), expected);
    }

    #[test]
    fn test_create_env_filter_reads_process_env() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _zshcs_guard = EnvVarGuard::set("ZSHCS_LOG", "debug");
        let _rust_guard = EnvVarGuard::remove("RUST_LOG");
        let filter = create_env_filter();
        assert_eq!(filter.to_string(), "debug");
    }

    #[test]
    fn test_init_logging_idempotent() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
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

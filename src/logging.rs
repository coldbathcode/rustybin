//! Tracing subscriber setup: `RUSTYBIN_LOG_LEVEL` (filter) and
//! `RUSTYBIN_LOG_FORMAT` (`text`, the default, or `json`).
//!
//! JSON lines carry the event fields at the top level (`timestamp`, `level`,
//! `target`, `message`, ...) plus a `span` object with the request span's
//! fields: `method`, `path` and `request_id` (the `X-Request-Id` value).

use tracing::Subscriber;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::config::Config;

/// `RUSTYBIN_LOG_FORMAT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Human readable lines (colours only on a terminal).
    #[default]
    Text,
    /// One JSON object per line.
    Json,
}

impl LogFormat {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" | "plain" | "pretty" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

/// Build the subscriber for a configuration, writing to `writer`.
pub fn subscriber<W>(config: &Config, writer: W, ansi: bool) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_new(&config.log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(writer);
    match config.log_format {
        LogFormat::Text => Box::new(builder.with_ansi(ansi).finish()),
        LogFormat::Json => Box::new(
            builder
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .finish(),
        ),
    }
}

/// Initialize the global tracing subscriber (stdout) with the configured
/// level and format. Safe to call twice.
pub fn init(config: &Config) {
    // Colour codes only on a terminal; container log collectors get plain text.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let _ = subscriber(config, std::io::stdout, ansi).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut b) = self.0.lock() {
                b.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buffer {
        type Writer = Buffer;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn parse_formats() {
        assert_eq!(LogFormat::parse("JSON"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("text"), Some(LogFormat::Text));
        assert_eq!(LogFormat::parse("xml"), None);
    }

    #[test]
    fn json_lines_carry_the_request_id() {
        let mut config = Config::for_tests();
        config.log_level = "info".to_string();
        config.log_format = LogFormat::Json;
        let buffer = Buffer::default();
        let _guard = tracing::subscriber::set_default(subscriber(&config, buffer.clone(), false));

        // The same span shape as the request span of `crate::build_app`
        // (tests/logging_json.rs checks the real server end to end).
        tracing::info_span!(
            "request",
            method = "GET",
            path = "/uuid",
            request_id = "log-test-123"
        )
        .in_scope(|| tracing::info!(status = 200, "finished processing request"));
        tracing::info!("a line outside any request");

        let raw = buffer.0.lock().map(|b| b.clone()).unwrap_or_default();
        let text = String::from_utf8(raw).expect("utf8");
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
            .collect();
        assert!(lines.len() >= 2, "{text}");
        let response_line = lines
            .iter()
            .find(|l| l["span"]["request_id"] == "log-test-123")
            .unwrap_or_else(|| panic!("no line with the request id: {text}"));
        assert_eq!(response_line["level"], "INFO");
        assert_eq!(response_line["span"]["path"], "/uuid");
        assert!(lines.iter().all(|l| l["timestamp"].is_string()));
    }
}

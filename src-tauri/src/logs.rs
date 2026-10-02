//! In-memory log buffer for the Logs screen. Every line passes through a
//! redactor that blanks registered secrets, so a token can never reach the UI
//! or the log file even if some library prints a URL containing it.

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::OnceLock;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

const MAX_LINES: usize = 2000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub seq: u64,
    /// Unix milliseconds.
    pub time: i64,
    pub level: &'static str,
    pub target: String,
    pub message: String,
}

type Sink = Box<dyn Fn(&LogLine) + Send + Sync>;

struct Buffer {
    lines: VecDeque<LogLine>,
    seq: u64,
}

static BUFFER: Mutex<Buffer> = Mutex::new(Buffer { lines: VecDeque::new(), seq: 0 });
static SECRETS: RwLock<Vec<String>> = RwLock::new(Vec::new());
static SINK: OnceLock<Sink> = OnceLock::new();

/// Blank this value wherever it appears in future log lines.
pub fn register_secret(value: &str) {
    let value = value.trim();
    if value.len() < 4 {
        return;
    }
    let mut secrets = SECRETS.write();
    if !secrets.iter().any(|s| s == value) {
        secrets.push(value.to_string());
    }
}

pub fn redact(text: &str) -> String {
    let secrets = SECRETS.read();
    let mut out = text.to_string();
    for s in secrets.iter() {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "<redacted>");
        }
    }
    out
}

/// Called for every new line (used to push lines to the UI).
pub fn set_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

pub fn snapshot() -> Vec<LogLine> {
    BUFFER.lock().lines.iter().cloned().collect()
}

pub fn clear() {
    BUFFER.lock().lines.clear();
}

pub struct BufferLayer;

#[derive(Default)]
struct Fields {
    message: String,
    extra: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.extra, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.extra, " {}={value}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let target = meta.target();
        if !target.starts_with("yarmiplayservertv") && *meta.level() > Level::WARN {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let message = redact(&format!("{}{}", fields.message, fields.extra));
        let level = match *meta.level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            Level::INFO => "info",
            Level::DEBUG => "debug",
            Level::TRACE => "trace",
        };
        let short_target = target.strip_prefix("yarmiplayservertv_lib::").unwrap_or(target).to_string();
        let line = {
            let mut buf = BUFFER.lock();
            buf.seq += 1;
            let line = LogLine {
                seq: buf.seq,
                time: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
                level,
                target: short_target,
                message,
            };
            buf.lines.push_back(line.clone());
            while buf.lines.len() > MAX_LINES {
                buf.lines.pop_front();
            }
            line
        };
        if let Some(sink) = SINK.get() {
            thread_local!(static IN_SINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
            if !IN_SINK.with(|f| f.replace(true)) {
                sink(&line);
                IN_SINK.with(|f| f.set(false));
            }
        }
    }
}

/// A `MakeWriter` for the stderr/file fmt layer that redacts secrets.
pub struct RedactingWriter<W: std::io::Write>(pub W);

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.0.write_all(redact(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub fn init() {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,yarmiplayservertv_lib=debug"));
    let fmt = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(|| RedactingWriter(std::io::stderr()));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt)
        .with(BufferLayer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_registered_secrets() {
        register_secret("super-secret-token-123");
        assert_eq!(
            redact("GET /update?token=super-secret-token-123&x=1"),
            "GET /update?token=<redacted>&x=1"
        );
        register_secret("ab");
        assert_eq!(redact("ab"), "ab", "very short values are ignored");
    }
}

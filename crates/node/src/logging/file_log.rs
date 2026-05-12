//! File-based structured logging layer for tracing.
//!
//! Writes JSON or text `LogEntry` records to a rotating log file.
//! A background tokio task handles all file I/O and rotation checks.

use std::fs::OpenOptions;
use std::io::Write;
use tokio::sync::mpsc;

use crate::logging::config::{LogConfig, LogFormat, LogOutput};
use crate::logging::entry::{LogEntry, LogLevel, LogValue};
use crate::logging::helpers::format_timestamp;
use crate::logging::rotation;

/// Start a background task that receives log entries and writes them to file.
pub(crate) fn start_file_logger_task(
    mut rx: mpsc::UnboundedReceiver<LogEntry>,
    config: LogConfig,
) -> Result<(), String> {
    let path = match &config.output {
        LogOutput::File(p) | LogOutput::Both(p) => p.clone(),
        LogOutput::Stdout => return Err("file logging requires a file path".into()),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    tokio::spawn(async move {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("failed to open log file");

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                Some(entry) = rx.recv() => {
                    let line = match config.format {
                        LogFormat::Json => entry.to_json(),
                        LogFormat::Text => entry.to_text(),
                    };
                    let _ = writeln!(file, "{}", line);
                    let _ = file.flush();
                }
                _ = interval.tick() => {
                    if rotation::should_rotate(&path, &config.rotation) {
                        let _ = rotation::rotate_log(&path);
                        if let Ok(new_file) = OpenOptions::new().create(true).append(true).open(&path) {
                            file = new_file;
                        }
                        let _ = rotation::cleanup_old_logs(&path, config.retention_days);
                    }
                }
            }
        }
    });

    Ok(())
}

/// Tracing layer that forwards events to a background file writer.
pub struct FileLogLayer {
    sender: mpsc::UnboundedSender<LogEntry>,
}

impl FileLogLayer {
    pub fn new(config: LogConfig) -> Result<Self, String> {
        let (tx, rx) = mpsc::unbounded_channel();
        start_file_logger_task(rx, config)?;
        Ok(Self { sender: tx })
    }
}

struct FieldVisitor {
    message: String,
    fields: std::collections::HashMap<String, LogValue>,
}

impl Default for FieldVisitor {
    fn default() -> Self {
        Self {
            message: String::new(),
            fields: std::collections::HashMap::new(),
        }
    }
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let val = format!("{:?}", value);
        if field.name() == "message" {
            self.message = val;
        } else {
            self.fields
                .insert(field.name().to_string(), LogValue::String(val));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.insert(
                field.name().to_string(),
                LogValue::String(value.to_string()),
            );
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), LogValue::Number(value as f64));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), LogValue::Number(value as f64));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), LogValue::Bool(value));
    }
}

impl<S> tracing_subscriber::Layer<S> for FileLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let level = match *event.metadata().level() {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let entry = LogEntry {
            timestamp: format_timestamp(now),
            level,
            target: event.metadata().target().to_string(),
            message: visitor.message,
            fields: visitor.fields,
        };

        let _ = self.sender.send(entry);
    }
}

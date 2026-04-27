use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::logging::helpers::format_timestamp;

/// JSON log value types
#[derive(Debug, Clone)]
pub enum LogValue {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

impl Serialize for LogValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            LogValue::String(s) => serializer.serialize_str(s),
            LogValue::Number(n) => serializer.serialize_f64(*n),
            LogValue::Bool(b) => serializer.serialize_bool(*b),
            LogValue::Null => serializer.serialize_unit(),
        }
    }
}

impl<'de> Deserialize<'de> for LogValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(LogValue::String(s)),
            serde_json::Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    Ok(LogValue::Number(f))
                } else {
                    Err(serde::de::Error::custom("invalid number"))
                }
            }
            serde_json::Value::Bool(b) => Ok(LogValue::Bool(b)),
            serde_json::Value::Null => Ok(LogValue::Null),
            _ => Err(serde::de::Error::custom("invalid log value")),
        }
    }
}

/// Structured log entry (per spec §24.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Log level
    pub level: LogLevel,
    /// Module path
    pub target: String,
    /// Log message
    pub message: String,
    /// Additional fields
    pub fields: HashMap<String, LogValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

impl LogEntry {
    pub fn new(level: LogLevel, target: &str, message: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Self {
            timestamp: format_timestamp(now),
            level,
            target: target.to_string(),
            message: message.to_string(),
            fields: HashMap::new(),
        }
    }

    pub fn with_field(mut self, key: &str, value: LogValue) -> Self {
        self.fields.insert(key.to_string(), value);
        self
    }

    /// Format as JSON string
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// Format as text line
    pub fn to_text(&self) -> String {
        format!(
            "{} [{}] {} {}",
            self.timestamp,
            self.level.as_str(),
            self.target,
            self.message
        )
    }
}

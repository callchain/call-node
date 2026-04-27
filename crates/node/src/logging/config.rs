use crate::logging::entry::LogLevel;
use std::path::PathBuf;

/// Log configuration (per spec §24.4)
#[derive(Debug, Clone)]
pub struct LogConfig {
    pub level: LogLevel,
    pub format: LogFormat,
    pub output: LogOutput,
    pub rotation: LogRotation,
    pub retention_days: u32,
    pub audit_enabled: bool,
    pub audit_path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub enum LogOutput {
    Stdout,
    File(PathBuf),
    Both(PathBuf),
}

#[derive(Debug, Clone)]
pub enum LogRotation {
    None,
    Size(u64), // bytes
    Daily,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            format: LogFormat::Text,
            output: LogOutput::Stdout,
            rotation: LogRotation::Size(100 * 1024 * 1024), // 100MB
            retention_days: 30,
            audit_enabled: true,
            audit_path: PathBuf::from(".callchain/audit.log"),
        }
    }
}

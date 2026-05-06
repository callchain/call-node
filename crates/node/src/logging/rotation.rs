use std::fs::File;
use std::path::Path;

use crate::logging::config::LogRotation;

/// Check if log file needs rotation based on config
pub fn should_rotate(path: &Path, rotation: &LogRotation) -> bool {
    match rotation {
        LogRotation::None => false,
        LogRotation::Size(max_bytes) => path
            .metadata()
            .map(|m| m.len() >= *max_bytes)
            .unwrap_or(false),
        LogRotation::Daily => path
            .metadata()
            .and_then(|m| m.modified())
            .map(|modified| {
                let now = std::time::SystemTime::now();
                now.duration_since(modified)
                    .map(|d| d.as_secs() >= 86_400)
                    .unwrap_or(false)
            })
            .unwrap_or(false),
    }
}

/// Rotate log file: rename current to .N, create new
pub fn rotate_log(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }

    // Find highest existing rotation number
    let mut max_n = 0;
    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let base = path.file_stem().and_then(|s| s.to_str()).unwrap_or("log");
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(suffix) = name.strip_prefix(&format!("{base}.")) {
                    if let Ok(n) = suffix.parse::<u32>() {
                        max_n = max_n.max(n);
                    }
                }
            }
        }
    }

    // Rotate: rename .N to .(N+1) in reverse order
    for n in (1..=max_n).rev() {
        let old = path.with_file_name(format!(
            "{}.{n}",
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("log")
        ));
        let new = path.with_file_name(format!(
            "{}.{}",
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("log"),
            n + 1
        ));
        if old.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }

    // Rename current to .1
    let rotated = path.with_file_name(format!(
        "{}.1",
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("log")
    ));
    std::fs::rename(path, &rotated).map_err(|e| format!("failed to rotate: {e}"))?;

    // Create new empty file
    File::create(path).map_err(|e| format!("failed to create new log: {e}"))?;

    Ok(())
}

/// Clean up old log files beyond retention period
pub fn cleanup_old_logs(path: &Path, retention_days: u32) -> Result<usize, String> {
    let mut removed = 0;
    let cutoff = retention_days as u64 * 86_400;

    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let base = path.file_stem().and_then(|s| s.to_str()).unwrap_or("log");
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&format!("{base}.")) {
                    if let Ok(metadata) = entry.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            let age = std::time::SystemTime::now()
                                .duration_since(modified)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            if age > cutoff {
                                let _ = std::fs::remove_file(entry.path());
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(removed)
}

use std::time::SystemTime;

pub fn format_timestamp(secs: u64) -> String {
    // Simplified ISO 8601 — full implementation would use chrono/jiff
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    let year = 1970 + days / 365;
    let day_of_year = days % 365;
    format!("{year}-01-{day_of_year:03}T{hours:02}:{mins:02}:{s:02}Z")
}

pub fn format_timestamp_for_compliance(t: &SystemTime) -> String {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| format_timestamp(d.as_secs()))
        .unwrap_or_default()
}

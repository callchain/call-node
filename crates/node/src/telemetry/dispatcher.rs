//! Alert dispatcher and background alert task.

use std::collections::HashSet;
use std::sync::RwLock;
use std::time::Duration;

use super::alert::{Alert, AlertSeverity, evaluate_alerts, default_alert_rules};
use super::registry::TelemetryRegistry;

// ── Alert Dispatcher ─────────────────────────────────────────────────

/// Dispatches triggered alerts to webhook and/or Slack
pub struct AlertDispatcher {
    pub webhook_url: Option<String>,
    pub slack_webhook_url: Option<String>,
    pub last_alert_names: RwLock<HashSet<String>>,
}

impl AlertDispatcher {
    pub fn new(webhook_url: Option<String>, slack_webhook_url: Option<String>) -> Self {
        Self {
            webhook_url,
            slack_webhook_url,
            last_alert_names: RwLock::new(HashSet::new()),
        }
    }

    pub async fn dispatch(&self, alert: &Alert) {
        if let Some(url) = &self.webhook_url {
            let _ = self.send_webhook(url, alert).await;
        }
        if let Some(url) = &self.slack_webhook_url {
            let _ = self.send_slack(url, alert).await;
        }
    }

    async fn send_webhook(&self, url: &str, alert: &Alert) -> Result<(), reqwest::Error> {
        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "alert": alert.name,
            "severity": format!("{:?}", alert.severity),
            "message": alert.message,
            "time": format!("{:?}", alert.triggered_at),
        });
        let body = serde_json::to_string(&payload).unwrap_or_default();
        client.post(url).header("Content-Type", "application/json").body(body).send().await?;
        Ok(())
    }

    async fn send_slack(&self, url: &str, alert: &Alert) -> Result<(), reqwest::Error> {
        let color = match alert.severity {
            AlertSeverity::Critical => "danger",
            AlertSeverity::Warning => "warning",
            AlertSeverity::Info => "good",
        };
        let ts = alert
            .triggered_at
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let payload = serde_json::json!({
            "attachments": [{
                "color": color,
                "title": format!("Callchain Alert: {}", alert.name),
                "text": alert.message,
                "footer": "callchain-telemetry",
                "ts": ts,
            }]
        });
        let body = serde_json::to_string(&payload).unwrap_or_default();
        reqwest::Client::new().post(url).header("Content-Type", "application/json").body(body).send().await?;
        Ok(())
    }
}

/// Start a background task that evaluates alert rules every 30 seconds
pub fn start_alert_task(
    registry: std::sync::Arc<TelemetryRegistry>,
    dispatcher: AlertDispatcher,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let rules = default_alert_rules();
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let alerts = evaluate_alerts(&registry, &rules);
            let last_names = dispatcher.last_alert_names.read().unwrap().clone();
            for alert in &alerts {
                if !last_names.contains(&alert.name) {
                    tracing::warn!(alert = %alert.name, severity = ?alert.severity, "ALERT triggered");
                    dispatcher.dispatch(alert).await;
                }
            }
            let mut names = dispatcher.last_alert_names.write().unwrap();
            names.clear();
            names.extend(alerts.iter().map(|a| a.name.clone()));
        }
    })
}

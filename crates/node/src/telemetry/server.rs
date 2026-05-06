//! HTTP /metrics and /health server.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Router,
};
use tokio::net::TcpListener;

use super::alert::db_heartbeat;
use super::alert::HealthState;
use super::registry::TelemetryRegistry;

/// Start the Prometheus /metrics HTTP server on the configured address.
/// Returns the bound address once the server is listening.
pub async fn start_metrics_server(
    registry: Arc<TelemetryRegistry>,
    health: HealthState,
    listen_addr: SocketAddr,
) -> Result<SocketAddr, std::io::Error> {
    let metrics_app = Router::new()
        .route("/metrics", axum::routing::get(metrics_handler))
        .with_state(registry);
    let health_app = Router::new()
        .route("/health", axum::routing::get(health_handler))
        .with_state(health);
    let app = metrics_app.merge(health_app);

    let listener = TcpListener::bind(listen_addr).await?;
    let bound = listener.local_addr()?;

    tokio::spawn(async move {
        tracing::info!("Metrics server listening on http://{bound}");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "metrics server error");
        }
    });

    Ok(bound)
}

async fn metrics_handler(State(registry): State<Arc<TelemetryRegistry>>) -> impl IntoResponse {
    let body = registry.prometheus_output();
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

async fn health_handler(State(state): State<HealthState>) -> impl IntoResponse {
    use axum::Json;
    let mut checks: HashMap<&str, String> = HashMap::new();
    let mut healthy = true;

    // DB check
    match db_heartbeat(&state.db) {
        Ok(()) => {
            checks.insert("db", "ok".to_string());
        }
        Err(e) => {
            healthy = false;
            checks.insert("db", e);
        }
    }

    // P2P check
    match &state.network {
        Some(net) if net.is_healthy() => {
            checks.insert("p2p", "ok".to_string());
        }
        Some(_) => {
            healthy = false;
            checks.insert("p2p", "no_peers".to_string());
        }
        None => {
            checks.insert("p2p", "disabled".to_string());
        }
    }

    // Sync check
    let height = state.consensus.read().unwrap().current_height();
    if height > 0 {
        checks.insert("sync", "ok".to_string());
    } else {
        healthy = false;
        checks.insert("sync", "not_started".to_string());
    }

    let status = if healthy { "healthy" } else { "degraded" };
    let body = serde_json::json!({ "status": status, "checks": checks });
    let code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body))
}

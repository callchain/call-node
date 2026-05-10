//! call-rpc — JSON-RPC server (per spec §11)

pub mod handlers;
pub mod keystore;
pub mod rate_limit;
pub mod standard;
pub mod state_bundle;
pub mod ws;

#[cfg(test)]
mod tests;

pub use handlers::*;
pub use keystore::LocalKeystore;
pub use rate_limit::RateLimiter;
pub use standard::*;
pub use state_bundle::*;
pub use ws::*;

use jsonrpsee::server::{
    serve_with_graceful_shutdown, stop_channel, Methods, Server, ServerHandle,
};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::RpcModule;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower::Layer;
use tower_http::cors::{Any, CorsLayer};

/// RPC server configuration
#[derive(Debug, Clone)]
pub struct RpcConfig {
    pub http_addr: SocketAddr,
    pub ws_addr: SocketAddr,
    pub max_connections: u32,
    /// Path to TLS certificate (PEM). If both `tls_cert_path` and `tls_key_path`
    /// are provided, the server serves HTTPS instead of HTTP.
    pub tls_cert_path: Option<String>,
    /// Path to TLS private key (PEM, PKCS#8).
    pub tls_key_path: Option<String>,
    /// Max requests per IP per window. `None` disables rate limiting.
    pub rate_limit_rps: Option<u64>,
    /// Rate-limit window in seconds (default: 60).
    pub rate_limit_window_secs: u64,
    /// CORS allowed origins. Empty = deny all non-localhost. `["*"]` = allow all.
    pub cors_allowed_origins: Vec<String>,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            http_addr: "127.0.0.1:8545".parse().unwrap(),
            ws_addr: "127.0.0.1:8546".parse().unwrap(),
            max_connections: 100,
            tls_cert_path: None,
            tls_key_path: None,
            rate_limit_rps: None,
            rate_limit_window_secs: 60,
            cors_allowed_origins: vec![],
        }
    }
}

/// Build a combined RPC module with all endpoints
pub fn build_rpc_module(
    state: Arc<RpcState>,
) -> Result<RpcModule<Arc<RpcState>>, ErrorObjectOwned> {
    let mut module = RpcModule::new(state.clone());
    standard::register_standard_rpc(&mut module)?;
    callchain::register_callchain_rpc(&mut module)?;
    ws::register_ws_subscriptions(&mut module, &state.subscriptions)?;
    Ok(module)
}

/// Start an HTTP/HTTPS JSON-RPC server with optional per-IP rate limiting.
///
/// Uses a custom accept loop so that:
/// - TLS can be applied at the connection level (`tokio-rustls`)
/// - per-IP rate limiting can drop connections before they reach jsonrpsee
/// Start an HTTP/HTTPS JSON-RPC server.
pub async fn start_http_server<Context>(
    config: RpcConfig,
    module: RpcModule<Context>,
) -> Result<(ServerHandle, SocketAddr), ErrorObjectOwned>
where
    Context: Send + Sync + 'static,
{
    let (handle, addr) = start_rpc_server(config.http_addr, &config, module, "HTTP").await?;
    Ok((handle, addr))
}

/// Start a WebSocket/WSS JSON-RPC server.
pub async fn start_ws_server<Context>(
    config: RpcConfig,
    module: RpcModule<Context>,
) -> Result<(ServerHandle, SocketAddr), ErrorObjectOwned>
where
    Context: Send + Sync + 'static,
{
    let (handle, addr) = start_rpc_server(config.ws_addr, &config, module, "WebSocket").await?;
    Ok((handle, addr))
}

async fn start_rpc_server<Context>(
    addr: SocketAddr,
    config: &RpcConfig,
    module: RpcModule<Context>,
    label: &str,
) -> Result<(ServerHandle, SocketAddr), ErrorObjectOwned>
where
    Context: Send + Sync + 'static,
{
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| ErrorObjectOwned::owned(-32603, format!("bind failed: {}", e), None::<()>))?;

    let bound_addr = listener.local_addr().map_err(|e| {
        ErrorObjectOwned::owned(-32603, format!("local_addr failed: {}", e), None::<()>)
    })?;

    let (stop_handle, server_handle) = stop_channel();

    let svc_builder = Server::builder()
        .max_connections(config.max_connections)
        .to_service_builder();

    let methods: Methods = module.into();

    let cors_layer = build_cors_layer(&config.cors_allowed_origins);

    // Optional TLS
    let tls_acceptor =
        if let (Some(ref cert), Some(ref key)) = (&config.tls_cert_path, &config.tls_key_path) {
            Some(build_tls_acceptor(cert, key).map_err(|e| {
                ErrorObjectOwned::owned(-32603, format!("TLS init failed: {}", e), None::<()>)
            })?)
        } else {
            None
        };

    // Optional rate limiter
    let rate_limiter = config
        .rate_limit_rps
        .map(|rps| RateLimiter::new(rps, config.rate_limit_window_secs));

    let is_https = tls_acceptor.is_some();
    let protocol = if is_https { "HTTPS" } else { "HTTP" };
    tracing::info!(
        label,
        addr = %addr,
        protocol,
        max_connections = config.max_connections,
        rate_limit_rps = config.rate_limit_rps,
        cors_origins = ?config.cors_allowed_origins,
        "{} RPC server started on {} ({})", label, addr, protocol
    );

    tokio::spawn(async move {
        loop {
            let (stream, remote_addr) = tokio::select! {
                res = listener.accept() => match res {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::error!("accept failed: {}", e);
                        continue;
                    }
                },
                _ = stop_handle.clone().shutdown() => break,
            };

            // Per-IP rate limiting at connection level
            if let Some(ref rl) = rate_limiter {
                if !rl.check(remote_addr.ip()) {
                    tracing::warn!(ip = %remote_addr, "rate limit exceeded, dropping connection");
                    drop(stream);
                    continue;
                }
            }

            let stop_handle2 = stop_handle.clone();
            let svc_builder2 = svc_builder.clone();
            let methods2 = methods.clone();
            let cors_conn = cors_layer.clone();

            if let Some(ref acceptor) = tls_acceptor {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            let svc = svc_builder2.build(methods2, stop_handle2.clone());
                            let svc = cors_conn.layer(svc);
                            let _ = serve_with_graceful_shutdown(
                                tls_stream,
                                svc,
                                stop_handle2.shutdown(),
                            )
                            .await;
                        }
                        Err(e) => {
                            tracing::warn!("TLS handshake failed: {}", e);
                        }
                    }
                });
            } else {
                tokio::spawn(async move {
                    let svc = svc_builder2.build(methods2, stop_handle2.clone());
                    let svc = cors_conn.layer(svc);
                    let _ =
                        serve_with_graceful_shutdown(stream, svc, stop_handle2.shutdown()).await;
                });
            }
        }
    });

    Ok((server_handle, bound_addr))
}

fn build_tls_acceptor(
    cert_path: &str,
    key_path: &str,
) -> Result<tokio_rustls::TlsAcceptor, String> {
    use std::fs::File;
    use std::io::BufReader;

    let cert_file =
        File::open(cert_path).map_err(|e| format!("open cert '{}': {}", cert_path, e))?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<rustls::Certificate> = rustls_pemfile::certs(&mut cert_reader)
        .map_err(|e| format!("parse cert: {}", e))?
        .into_iter()
        .map(rustls::Certificate)
        .collect();

    let key_file = File::open(key_path).map_err(|e| format!("open key '{}': {}", key_path, e))?;
    let mut key_reader = BufReader::new(key_file);
    let mut keys: Vec<rustls::PrivateKey> = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
        .map_err(|e| format!("parse key: {}", e))?
        .into_iter()
        .map(rustls::PrivateKey)
        .collect();

    // Fallback to RSA traditional format if no PKCS#8 keys found
    if keys.is_empty() {
        let mut key_reader2 =
            BufReader::new(File::open(key_path).map_err(|e| format!("re-open key: {}", e))?);
        keys = rustls_pemfile::rsa_private_keys(&mut key_reader2)
            .map_err(|e| format!("parse RSA key: {}", e))?
            .into_iter()
            .map(rustls::PrivateKey)
            .collect();
    }

    let key = keys
        .into_iter()
        .next()
        .ok_or("no private key found in key file")?;

    let config = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("invalid cert/key: {}", e))?;

    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// Build a `CorsLayer` from the configured allowed origins.
///
/// - `["*"]` allows any origin.
/// - Empty allows only localhost / 127.x.x.x origins.
/// - Specific origins are matched exactly.
fn build_cors_layer(origins: &[String]) -> CorsLayer {
    use http::header::{self, HeaderValue};
    use tower_http::cors::AllowOrigin;

    if origins.iter().any(|o| o == "*") {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([http::Method::POST, http::Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE])
    } else if origins.is_empty() {
        CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(|origin, _request_parts| {
                if let Some(s) = origin.to_str().ok() {
                    s.starts_with("http://localhost")
                        || s.starts_with("https://localhost")
                        || s.starts_with("http://127.")
                        || s.starts_with("https://127.")
                } else {
                    false
                }
            }))
            .allow_methods([http::Method::POST, http::Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE])
    } else {
        let allowed: Vec<HeaderValue> = origins
            .iter()
            .filter_map(|o| HeaderValue::from_str(o).ok())
            .collect();
        CorsLayer::new()
            .allow_origin(allowed)
            .allow_methods([http::Method::POST, http::Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE])
    }
}

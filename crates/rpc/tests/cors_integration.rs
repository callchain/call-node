//! CORS integration tests for the JSON-RPC server.
//!
//! Verifies:
//! - Preflight (OPTIONS) requests are allowed for configured origins
//! - Preflight requests are denied for non-configured origins
//! - Actual POST requests from allowed origins receive ACAO headers
//! - Wildcard (`["*"]`) allows any origin
//! - Empty origin list defaults to localhost-only

use call_evm::provider::InMemoryStateProvider;
use call_mempool::Mempool;
use call_rpc::handlers::RpcState;
use call_rpc::{build_rpc_module, start_http_server, RpcConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ── Helpers ──────────────────────────────────────────────────────────

fn make_test_db() -> (std::path::PathBuf, Arc<reth_db::DatabaseEnv>) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "call-rpc-cors-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    let db = call_storage::reth_db::init_call_db(&tmp).expect("init test db");
    let evm = InMemoryStateProvider::new();
    evm.save_to_db(&db).expect("seed test db");
    (tmp, db)
}

fn make_test_state() -> RpcState {
    let (_tmp, db) = make_test_db();
    let mempool = Arc::new(std::sync::RwLock::new(Mempool::new()));
    RpcState::new(db, mempool, 1)
}

async fn send_http_request(
    addr: SocketAddr,
    request: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .map_err(|_| "request timed out")??;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn parse_response_headers(response: &str) -> Vec<(&str, &str)> {
    let mut headers = Vec::new();
    for line in response.lines() {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(": ") {
            headers.push((k, v));
        }
    }
    headers
}

fn has_header_contains(headers: &[(&str, &str)], name: &str, substr: &str) -> bool {
    headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case(name) && v.contains(substr))
}

fn is_success_status(response: &str) -> bool {
    response.contains("HTTP/1.1 200") || response.contains("HTTP/1.1 204")
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cors_preflight_allowed_origin() {
    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: None,
        tls_key_path: None,
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec!["http://example.com".to_string()],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    let request = format!(
        "OPTIONS / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Origin: http://example.com\r\n\
         Access-Control-Request-Method: POST\r\n\
         Access-Control-Request-Headers: Content-Type\r\n\
         \r\n",
        addr.port()
    );
    let response = send_http_request(addr, &request).await.unwrap();

    assert!(
        is_success_status(&response),
        "Expected successful preflight, got: {}",
        response.lines().next().unwrap_or("empty")
    );
    let headers = parse_response_headers(&response);
    assert!(
        has_header_contains(
            &headers,
            "access-control-allow-origin",
            "http://example.com"
        ),
        "Expected ACAO header for allowed origin"
    );

    server.stop().unwrap();
}

#[tokio::test]
async fn test_cors_preflight_denied_origin() {
    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: None,
        tls_key_path: None,
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec!["http://example.com".to_string()],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    let request = format!(
        "OPTIONS / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Origin: http://evil.com\r\n\
         Access-Control-Request-Method: POST\r\n\
         \r\n",
        addr.port()
    );
    let response = send_http_request(addr, &request).await.unwrap();

    // tower-http returns 200 for denied preflight; the absence of ACAO is the signal
    let headers = parse_response_headers(&response);
    assert!(
        !has_header_contains(&headers, "access-control-allow-origin", "evil.com"),
        "Expected no ACAO header for denied origin, got: {}",
        response.lines().next().unwrap_or("empty")
    );

    server.stop().unwrap();
}

#[tokio::test]
async fn test_cors_actual_request_allowed() {
    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: None,
        tls_key_path: None,
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec!["http://example.com".to_string()],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    let request = format!(
        "POST / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Origin: http://example.com\r\n\
         Content-Type: application/json\r\n\
         Content-Length: 59\r\n\
         \r\n\
         {{\"jsonrpc\":\"2.0\",\"method\":\"eth_chainId\",\"params\":[],\"id\":1}}",
        addr.port()
    );
    let response = send_http_request(addr, &request).await.unwrap();

    assert!(
        response.contains("HTTP/1.1 200"),
        "Expected 200 for allowed actual request, got: {}",
        response.lines().next().unwrap_or("empty")
    );
    let headers = parse_response_headers(&response);
    assert!(
        has_header_contains(
            &headers,
            "access-control-allow-origin",
            "http://example.com"
        ),
        "Expected ACAO header for allowed origin"
    );

    server.stop().unwrap();
}

#[tokio::test]
async fn test_cors_actual_request_denied() {
    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: None,
        tls_key_path: None,
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec!["http://example.com".to_string()],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    let request = format!(
        "POST / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Origin: http://evil.com\r\n\
         Content-Type: application/json\r\n\
         Content-Length: 59\r\n\
         \r\n\
         {{\"jsonrpc\":\"2.0\",\"method\":\"eth_chainId\",\"params\":[],\"id\":1}}",
        addr.port()
    );
    let response = send_http_request(addr, &request).await.unwrap();

    assert!(
        response.contains("HTTP/1.1 200"),
        "Expected 200 even for denied origin (request still processes)"
    );
    let headers = parse_response_headers(&response);
    assert!(
        !has_header_contains(&headers, "access-control-allow-origin", "evil.com"),
        "Expected no ACAO header for denied origin"
    );

    server.stop().unwrap();
}

#[tokio::test]
async fn test_cors_wildcard_allows_any() {
    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: None,
        tls_key_path: None,
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec!["*".to_string()],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    let request = format!(
        "OPTIONS / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Origin: http://anything.com\r\n\
         Access-Control-Request-Method: POST\r\n\
         \r\n",
        addr.port()
    );
    let response = send_http_request(addr, &request).await.unwrap();

    assert!(
        is_success_status(&response),
        "Expected successful preflight for wildcard, got: {}",
        response.lines().next().unwrap_or("empty")
    );
    let headers = parse_response_headers(&response);
    assert!(
        has_header_contains(&headers, "access-control-allow-origin", "*"),
        "Expected ACAO: * for wildcard config"
    );

    server.stop().unwrap();
}

#[tokio::test]
async fn test_cors_empty_origins_allows_localhost() {
    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: None,
        tls_key_path: None,
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec![],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    // localhost origin should be allowed
    let request = format!(
        "OPTIONS / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Origin: http://localhost:3000\r\n\
         Access-Control-Request-Method: POST\r\n\
         \r\n",
        addr.port()
    );
    let response = send_http_request(addr, &request).await.unwrap();

    assert!(
        is_success_status(&response),
        "Expected successful preflight for localhost, got: {}",
        response.lines().next().unwrap_or("empty")
    );
    let headers = parse_response_headers(&response);
    assert!(
        has_header_contains(
            &headers,
            "access-control-allow-origin",
            "http://localhost:3000"
        ),
        "Expected ACAO header for localhost origin"
    );

    // Non-localhost origin should be denied
    let request2 = format!(
        "OPTIONS / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Origin: http://evil.com\r\n\
         Access-Control-Request-Method: POST\r\n\
         \r\n",
        addr.port()
    );
    let response2 = send_http_request(addr, &request2).await.unwrap();

    // tower-http returns 200 for denied preflight; the absence of ACAO is the signal
    let headers2 = parse_response_headers(&response2);
    assert!(
        !has_header_contains(&headers2, "access-control-allow-origin", "evil.com"),
        "Expected no ACAO header for non-localhost origin, got: {}",
        response2.lines().next().unwrap_or("empty")
    );

    server.stop().unwrap();
}

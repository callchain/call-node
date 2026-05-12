//! TLS integration tests for the JSON-RPC server.
//!
//! Verifies:
//! - TLS handshake succeeds with a valid self-signed certificate
//! - Plain HTTP connections are dropped on a TLS-enabled port
//! - Expired certificates cause the TLS handshake to fail

use call_evm::provider::InMemoryStateProvider;
use call_mempool::Mempool;
use call_rpc::handlers::RpcState;
use call_rpc::{build_rpc_module, start_http_server, RpcConfig};
use rcgen::{CertificateParams, KeyPair, SanType};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

// ── Helpers ──────────────────────────────────────────────────────────

fn make_test_db() -> (std::path::PathBuf, Arc<reth_db::DatabaseEnv>) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "call-rpc-tls-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::SeqCst),
    ));
    let db = call_storage::reth_db::init_call_db_test(&tmp).expect("init test db");
    let evm = InMemoryStateProvider::new();
    evm.save_to_db(&db).expect("seed test db");
    (tmp, db)
}

fn make_test_state() -> RpcState {
    let (_tmp, db) = make_test_db();
    let mempool = Arc::new(std::sync::RwLock::new(Mempool::new()));
    RpcState::new(db, mempool, 1)
}

fn generate_test_cert() -> (Vec<u8>, Vec<u8>) {
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::DnsName("localhost".try_into().unwrap())];
    let key_pair = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();
    (cert_pem, key_pem)
}

fn generate_expired_cert() -> (Vec<u8>, Vec<u8>) {
    use time::{Duration, OffsetDateTime};
    let mut params = CertificateParams::default();
    params.not_before = OffsetDateTime::now_utc() - Duration::days(30);
    params.not_after = OffsetDateTime::now_utc() - Duration::days(1);
    params.subject_alt_names = vec![SanType::DnsName("localhost".try_into().unwrap())];
    let key_pair = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();
    (cert_pem, key_pem)
}

fn write_certs(
    temp_dir: &tempfile::TempDir,
    cert_pem: &[u8],
    key_pem: &[u8],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let cert_path = temp_dir.path().join("test.crt");
    let key_path = temp_dir.path().join("test.key");
    std::fs::write(&cert_path, cert_pem).unwrap();
    std::fs::write(&key_path, key_pem).unwrap();
    (cert_path, key_path)
}

/// Build a rustls `ClientConfig` that trusts the provided PEM certificate.
fn trust_cert_config(cert_pem: &[u8]) -> ClientConfig {
    use rustls::pki_types::CertificateDer;

    let mut roots = RootCertStore::empty();
    let mut reader = std::io::BufReader::new(cert_pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for cert in &certs {
        roots.add(cert.clone()).unwrap();
    }
    ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Send a raw JSON-RPC request over a TLS stream and return the HTTP response body.
async fn send_jsonrpc_request(
    addr: SocketAddr,
    client_config: ClientConfig,
) -> Result<String, Box<dyn std::error::Error>> {
    let connector = TlsConnector::from(Arc::new(client_config));
    let stream = TcpStream::connect(addr).await?;
    let domain = ServerName::try_from("localhost")?;
    let mut tls_stream = connector.connect(domain, stream).await?;

    let request = format!(
        "POST / HTTP/1.1\r\n\
         Host: localhost:{}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: 59\r\n\
         \r\n\
         {{\"jsonrpc\":\"2.0\",\"method\":\"eth_chainId\",\"params\":[],\"id\":1}}",
        addr.port()
    );
    tls_stream.write_all(request.as_bytes()).await?;
    tls_stream.flush().await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), tls_stream.read(&mut buf))
        .await
        .map_err(|_| "TLS request timed out")??;
    buf.truncate(n);

    let response = String::from_utf8_lossy(&buf);
    if let Some(idx) = response.find("\r\n\r\n") {
        Ok(response[idx + 4..].to_string())
    } else {
        Ok(response.to_string())
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tls_handshake_success() {
    let (cert_pem, key_pem) = generate_test_cert();
    let temp_dir = tempfile::tempdir().unwrap();
    let (cert_path, key_path) = write_certs(&temp_dir, &cert_pem, &key_pem);

    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: Some(cert_path.to_str().unwrap().to_string()),
        tls_key_path: Some(key_path.to_str().unwrap().to_string()),
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec![],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    let client_config = trust_cert_config(&cert_pem);
    let body = send_jsonrpc_request(addr, client_config).await.unwrap();

    assert!(
        body.contains("jsonrpc") || body.contains("result") || body.contains("error"),
        "Expected a JSON-RPC response, got: {}",
        body
    );

    server.stop().unwrap();
}

#[tokio::test]
async fn test_tls_rejects_plain_http() {
    let (cert_pem, key_pem) = generate_test_cert();
    let temp_dir = tempfile::tempdir().unwrap();
    let (cert_path, key_path) = write_certs(&temp_dir, &cert_pem, &key_pem);

    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: Some(cert_path.to_str().unwrap().to_string()),
        tls_key_path: Some(key_path.to_str().unwrap().to_string()),
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec![],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    // Connect with plain TCP (no TLS) and send HTTP bytes
    let mut stream: TcpStream = TcpStream::connect(addr).await.unwrap();
    let request: &[u8] = b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n";
    stream.write_all(request).await.unwrap();
    stream.flush().await.unwrap();

    let mut buf = [0u8; 256];
    let result = stream.read(&mut buf).await;
    match result {
        Ok(0) => {}
        Ok(n) => {
            let response = String::from_utf8_lossy(&buf[..n]);
            assert!(
                !response.contains("HTTP/1.1"),
                "Server sent an HTTP response on a TLS port: {}",
                response
            );
        }
        Err(_) => {}
    }

    server.stop().unwrap();
}

#[tokio::test]
async fn test_tls_expired_cert_rejected() {
    let (cert_pem, key_pem) = generate_expired_cert();
    let temp_dir = tempfile::tempdir().unwrap();
    let (cert_path, key_path) = write_certs(&temp_dir, &cert_pem, &key_pem);

    let config = RpcConfig {
        http_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        max_connections: 100,
        tls_cert_path: Some(cert_path.to_str().unwrap().to_string()),
        tls_key_path: Some(key_path.to_str().unwrap().to_string()),
        rate_limit_rps: None,
        rate_limit_window_secs: 60,
        cors_allowed_origins: vec![],
    };

    let state = Arc::new(make_test_state());
    let module = build_rpc_module(state).unwrap();
    let (server, addr) = start_http_server(config, module).await.unwrap();

    let client_config = trust_cert_config(&cert_pem);
    let result = send_jsonrpc_request(addr, client_config).await;

    assert!(
        result.is_err(),
        "Expected TLS handshake to fail with expired certificate"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("certificate") || err.contains("Certificate"),
        "Expected certificate error, got: {}",
        err
    );

    server.stop().unwrap();
}

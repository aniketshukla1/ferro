//! B8a acceptance: `--read-only`, audit log, TLS (ORA-26).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use ferro_server::contract::{api_contract_routes, contract_is_read};
use ferro_server::guard::GuardConfig;
use ferro_server::server;
use ferro_server::tls;
use rustls::pki_types::pem::PemObject;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const FAKE_SECRET: &str = "FAKE-AUDIT-CREDENTIAL-xyzzy-999";

fn workspace_router(read_only: bool) -> (axum::Router, tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        read_only,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    (server::build_router(st, guard, last, true, None), dir, home)
}

fn api_req(method: Method, path: &str, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(Body::from(body.unwrap_or("").to_string())).unwrap()
}

fn sample_path(template: &str) -> String {
    template
        .replace("{id}", "sample")
        .replace("[/{id}]", "/sample")
}

async fn wait_tcp(port: u16) -> tokio::net::TcpStream {
    for _ in 0..50 {
        if let Ok(stream) = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await {
            return stream;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for ferro on port {port}");
}

#[tokio::test]
async fn read_only_blocks_every_contract_mutation() {
    let (app, _d, _h) = workspace_router(true);
    for route in api_contract_routes().into_iter().filter(|r| r.mutating) {
        let path = sample_path(&route.path);
        let req = api_req(route.method.clone(), &path, Some("{}"));
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::FORBIDDEN,
            "{} {}",
            route.method,
            path
        );
    }
}

#[tokio::test]
async fn read_only_allows_contract_reads() {
    let (app, _d, _h) = workspace_router(true);
    for route in api_contract_routes().into_iter().filter(|r| !r.mutating) {
        let path = sample_path(&route.path);
        assert!(
            contract_is_read(&route.method, &path),
            "sampled read path is not on the contract table: {} {path}",
            route.method
        );
        let req = api_req(route.method.clone(), &path, None);
        let res = app.clone().oneshot(req).await.unwrap();
        assert_ne!(
            res.status(),
            StatusCode::FORBIDDEN,
            "{} {}",
            route.method,
            path
        );
    }
}

#[tokio::test]
async fn audit_log_records_mutation_without_body_secrets() {
    let (app, _d, home) = workspace_router(false);
    let state_dir = home.path().join("s");
    let log_path = state_dir.join("audit.jsonl");
    let body = format!(r#"{{"values":{{"ui.test":true,"ai.neverSend":["{FAKE_SECRET}"]}}}}"#);
    let req = api_req(Method::PUT, "/api/v1/settings?scope=user", Some(&body));
    let res = app.oneshot(req).await.unwrap();
    assert!(res.status().is_success());
    let text = std::fs::read_to_string(&log_path).expect("audit.jsonl");
    let lines: Vec<_> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "one JSONL line per mutation: {text}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(v.get("timestamp").and_then(|t| t.as_str()).is_some());
    assert!(v.get("route").is_some());
    assert!(v.get("actor").is_some());
    assert!(v.get("target").is_some());
    assert!(v.get("body").is_none());
    assert!(
        !text.contains(FAKE_SECRET),
        "audit log must not contain request secrets"
    );
    assert!(!text.contains(TOKEN));
}

#[tokio::test]
async fn read_only_is_reported_on_meta() {
    let (app, _d, _h) = workspace_router(true);
    let res = app
        .oneshot(api_req(Method::GET, "/api/v1/meta", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["readOnly"], true);
    assert_eq!(v["specVersion"], "1.11");

    let (app, _d, _h) = workspace_router(false);
    let res = app
        .oneshot(api_req(Method::GET, "/api/v1/meta", None))
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["readOnly"], false);
}

#[tokio::test]
async fn read_only_blocks_legacy_api_writes() {
    let (app, _d, _h) = workspace_router(true);
    for (method, path) in [
        (Method::POST, "/api/review/drafts"),
        (Method::POST, "/api/ask"),
        (Method::POST, "/api/stats"),
    ] {
        let req = api_req(method.clone(), path, Some("{}"));
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} {path}");
    }
}

#[tokio::test]
async fn a_dropped_handle_keeps_serving() {
    // The desktop app keeps only the port and token: dropping the handle must not stop ferro.
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let o = server::ServeOpts {
        host: "127.0.0.1".into(),
        port: 0,
        no_git: true,
        narrate: false,
        no_open: true,
        initial: None,
        token: Some(TOKEN.into()),
        allow_hosts: vec![],
        no_auth: false,
        dev_web: None,
        read_only: false,
        tls: tls::TlsConfig::None,
        tls_fingerprint: None,
        base_path: None,
        no_lsp: true,
    };
    let port = server::serve(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
        o,
    )
    .await
    .port;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut stream = wait_tcp(port).await;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = format!(
        "GET /api/v1/meta HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf);
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
}

#[tokio::test]
async fn tls_self_signed_serves_https_and_refuses_plain_http() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let state_dir = home.path().join("s");
    let (_generated, cert_path, key_path) = tls::self_signed(state_dir.as_path()).unwrap();
    // `--tls-cert` / `--tls-key` load the same PEM files self-signed writes.
    let loaded = tls::load_pem_files(&cert_path, &key_path).unwrap();
    let fp = loaded.fingerprint.clone().expect("fingerprint");
    assert_eq!(
        tls::fingerprint_announcement(&fp),
        format!("ferro tls fingerprint SHA-256: {fp}")
    );
    let o = server::ServeOpts {
        host: "127.0.0.1".into(),
        port: 0,
        no_git: true,
        narrate: true,
        no_open: true,
        initial: None,
        token: Some(TOKEN.into()),
        allow_hosts: vec![],
        no_auth: false,
        dev_web: None,
        read_only: false,
        tls: loaded.config,
        tls_fingerprint: loaded.fingerprint,
        base_path: None,
        no_lsp: false,
    };
    let h = server::serve(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
        o,
    )
    .await;
    assert!(h.url.starts_with("https://"));

    let mut root_store = rustls::RootCertStore::empty();
    let cert_pem = std::fs::read(&cert_path).unwrap();
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls::pki_types::CertificateDer::pem_slice_iter(&cert_pem)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    let leaf = certs[0].as_ref().to_vec();
    for c in &certs {
        root_store.add(c.clone()).unwrap();
    }
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
    let stream = wait_tcp(h.port).await;
    let mut tls_stream = connector
        .connect(
            rustls::pki_types::ServerName::try_from("localhost".to_string()).unwrap(),
            stream,
        )
        .await
        .unwrap();
    let req = format!(
        "GET /api/v1/meta HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        h.port, TOKEN
    );
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tls_stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tls_stream.read_to_end(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf);
    assert!(resp.contains("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("\"specVersion\":\"1.11\""), "{resp}");
    assert!(resp.contains("\"readOnly\":false"), "{resp}");
    drop(tls_stream);

    let served_fp = tls::cert_fingerprint(&leaf);
    assert_eq!(served_fp, fp);

    // Plain HTTP on the TLS port is refused. It must not be answered as HTTP.
    {
        let plain = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", h.port))
            .await
            .unwrap();
        let (mut read, mut write) = plain.into_split();
        write
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut plain_buf = vec![0u8; 64];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), read.read(&mut plain_buf))
            .await;
        match n {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => {}
            Ok(Ok(n)) => {
                let head = String::from_utf8_lossy(&plain_buf[..n]);
                assert!(
                    !head.starts_with("HTTP/"),
                    "plain HTTP must be refused on the TLS port, not downgraded: {head}"
                );
            }
        }
    }
    let port = h.port;
    let _ = h.shutdown.send(());
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(3), async move {
        loop {
            if tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(stopped.is_ok(), "TLS server ignored shutdown");
}

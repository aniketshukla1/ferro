//! Opening folders (API.md § 7.2): `POST /workspace/open` takes an absolute path or `~/…`, and
//! `GET /workspace/recent` lists folders opened before, newest first, flagging the current one.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn folder(parent: &Path, name: &str) -> PathBuf {
    let p = parent.join(name);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("README.md"), format!("# {name}\n")).unwrap();
    p.canonicalize().unwrap()
}

fn router(st: &Arc<ferro_server::state::AppState>, read_only: bool) -> axum::Router {
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        read_only,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    server::build_router(st.clone(), guard, last, false, None)
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let req = b
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn roots(v: &serde_json::Value) -> Vec<(String, bool)> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["root"].as_str().unwrap().to_string(),
                i["current"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn open_folders_and_list_them_newest_first() {
    let tmp = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let (a, b, c) = (
        folder(tmp.path(), "api"),
        folder(tmp.path(), "web"),
        folder(tmp.path(), "gone"),
    );
    let dirs = ferro_core::dirs::FerroDirs::new(
        tmp.path().join("cfg"),
        tmp.path().join("state"),
        tmp.path().join("cache"),
    );
    let st = server::build_state(
        a.clone(),
        dirs.clone(),
        ferro_server::Host::Cli,
        "test".into(),
    );
    ferro_server::recent::record(&dirs, &a); // what `serve` does at startup
    let s = |p: &Path| p.to_string_lossy().into_owned();

    let (code, v) = call(router(&st, false), "GET", "/api/v1/workspace/recent", None).await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(roots(&v), vec![(s(&a), true)]);
    assert_eq!(v["items"][0]["name"], "api");

    let (code, v) = call(
        router(&st, false),
        "POST",
        "/api/v1/workspace/open",
        Some(serde_json::json!({ "path": s(&b) })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(st.ws().root, b);
    let (_, v) = call(router(&st, false), "GET", "/api/v1/workspace/recent", None).await;
    assert_eq!(roots(&v), vec![(s(&b), true), (s(&a), false)]);

    // A folder that is gone drops out of the list.
    call(
        router(&st, false),
        "POST",
        "/api/v1/workspace/open",
        Some(serde_json::json!({ "path": s(&c) })),
    )
    .await;
    call(
        router(&st, false),
        "POST",
        "/api/v1/workspace/open",
        Some(serde_json::json!({ "path": s(&a) })),
    )
    .await;
    std::fs::remove_dir_all(&c).unwrap();
    let (_, v) = call(router(&st, false), "GET", "/api/v1/workspace/recent", None).await;
    assert_eq!(roots(&v), vec![(s(&a), true), (s(&b), false)]);

    // Relative paths are refused (never resolved against the server's cwd); files are not folders.
    let (code, v) = call(
        router(&st, false),
        "POST",
        "/api/v1/workspace/open",
        Some(serde_json::json!({ "path": "web" })),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{v}");
    let (code, _) = call(
        router(&st, false),
        "POST",
        "/api/v1/workspace/open",
        Some(serde_json::json!({ "path": s(&a.join("README.md")) })),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(st.ws().root, a, "a refused open leaves the workspace alone");

    // Read-only: no switching, and no list of other folders on the host.
    let (code, v) = call(router(&st, true), "GET", "/api/v1/workspace/recent", None).await;
    assert_eq!(
        (code, v["items"].as_array().unwrap().len()),
        (StatusCode::OK, 0)
    );
    let (code, _) = call(
        router(&st, true),
        "POST",
        "/api/v1/workspace/open",
        Some(serde_json::json!({ "path": s(&b) })),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
}

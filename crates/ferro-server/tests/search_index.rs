//! B2b: trigram index equivalence with the scan engine, freshness, policy.
//! 50-query set comparing file:line hits; engine field; delta/tombstones.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::collections::BTreeSet;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn state_with_files(files: &[(&str, &str)]) -> (axum::Router, Arc<ferro_server::state::AppState>) {
    let dir = tempfile::tempdir().unwrap();
    for (p, content) in files {
        let full = dir.path().join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
    }
    let dir = Box::leak(Box::new(dir));
    let home = Box::leak(Box::new(tempfile::tempdir().unwrap()));
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
    // Synchronous file index so the snapshot exists.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let idx = st.ws().index.clone();
                rt.block_on(idx.rebuild());
            })
            .join()
            .unwrap();
    });
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    (
        server::build_router(st.clone(), guard, last, true, None),
        st,
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

async fn j(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = router.oneshot(get(uri)).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, v)
}

fn hit_set(v: &serde_json::Value) -> BTreeSet<(String, u64)> {
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| {
            let p = f["path"].as_str().unwrap().to_string();
            f["hits"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |h| (p.clone(), h["line"].as_u64().unwrap()))
        })
        .collect()
}

fn fixture_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "src/main.rs",
            "fn main() {\n    serve(8080);\n    println!(\"hi\");\n}\n",
        ),
        (
            "src/lib.rs",
            "pub fn serve(port: u16) {}\n// Serve the world\npub const FOO: &str = \"foo\";\n",
        ),
        (
            "src/uni.rs",
            "// naïve café\nfn café() {}\nlet party = \"🎉\";\n",
        ),
        (
            "web/app.js",
            "function serve() { return 1; }\nconst SERVER = serve;\n",
        ),
        ("docs/r.md", "# Serve\n\nRun `serve --port 80` now.\n"),
        ("pkg/n.go", "package n\nfunc Serve() {}\n// serve serve\n"),
        ("data/words.txt", "alpha beta gamma\ndelta epsilon\n"),
        ("empty.txt", ""),
    ]
}

/// Build the trigram index synchronously through the public policy path.
fn build_now(st: &Arc<ferro_server::state::AppState>) {
    let ws = st.ws();
    // Narrow policy: force mode on via settings, then ensure.
    st.settings
        .save(
            &ws.key,
            "workspace",
            [("search.index".into(), serde_json::json!("on"))]
                .into_iter()
                .collect(),
        )
        .unwrap();
    st.ensure_search_built();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if ws.search.state() == ferro_core::trigram::SearchIndexState::Ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "index never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

const QUERIES: &[&str] = &[
    "q=serve",
    "q=Serve",
    "q=SERVE&case=sensitive",
    "q=serve&case=sensitive",
    "q=serve&word=1",
    "q=foo",
    "q=fn",
    "q=fn&word=1",
    "q=café",
    "q=party",
    "q=world",
    "q=80",
    "q=serve&include=src%2F**",
    "q=serve&exclude=src%2F**",
    "q=zzqqxx_no_such_token",
    "q=return",
    "q=a",
    "q=ab",
    "q=alpha",
    "q=epsilon",
    "q=main",
    "q=const",
    "q=package",
    "q=func",
    "q=println",
    "q=8080",
    "q=port",
    "q=mode%3Aliteral&mode=literal",
    "q=ser&mode=regex",
    "q=fn+%5Cw%2B%5C(&mode=regex",
    "q=serve%7CServe&mode=regex&case=sensitive",
    "q=%5Cbfoo%5Cb&mode=regex",
    "q=caf.&mode=regex",
    "q=%5Epub&mode=regex",
    "q=serve%24&mode=regex",
    "q=%5Cd%2B&mode=regex",
    "q=%5Cw%2B&mode=regex",
    "q=s%5Ba-z%5D%2Bve&mode=regex",
    "q=(serve%7Cmain)&mode=regex",
    "q=se(r%7Cv)e&mode=regex",
    "q=na%C3%AFve",
    "q=%F0%9F%8E%89",
    "q=%22foo%22",
    "q=%3A%3A",
    "q=--port",
    "q=%60serve",
    "q=txt&include=**%2F*.txt",
    "q=serve&include=src%2F**&exclude=**%2Flib.rs",
    "q=e",
    "q=the",
];

#[tokio::test]
async fn index_matches_scan_on_50_queries() {
    let (app, st) = state_with_files(&fixture_files());
    build_now(&st);
    let (s, m) = j(app.clone(), "/api/v1/meta").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m["index"]["searchIndex"], "ready");
    let ws = st.ws();
    let snap = ws.index.file_index.load();
    let stop = std::sync::atomic::AtomicBool::new(false);
    for (i, qs) in QUERIES.iter().enumerate() {
        let uri = format!("/api/v1/search?{qs}&maxFiles=200&maxPerFile=1000");
        let (s, v) = j(app.clone(), &uri).await;
        assert_eq!(s, StatusCode::OK, "{qs}: {v}");
        // Reference: the scan engine over the same snapshot.
        let mut q = ferro_core::scan::Query::literal("x");
        // Rebuild the query the way the endpoint does (subset of params).
        let get = |k: &str| {
            qs.split('&').find_map(|p| {
                let (kk, vv) = p.split_once('=')?;
                (kk == k).then(|| vv.to_string())
            })
        };
        let pat = urlencoding_decode(&get("q").unwrap_or_default());
        if pat.is_empty() {
            continue;
        }
        q.pattern = pat;
        q.mode = if get("mode").as_deref() == Some("regex") {
            ferro_core::scan::Mode::Regex
        } else {
            ferro_core::scan::Mode::Literal
        };
        q.case = match get("case").as_deref() {
            Some("sensitive") => ferro_core::scan::Case::Sensitive,
            Some("insensitive") => ferro_core::scan::Case::Insensitive,
            _ => ferro_core::scan::Case::Smart,
        };
        q.word = get("word").as_deref() == Some("1");
        q.include = get("include")
            .map(|g| vec![urlencoding_decode(&g)])
            .unwrap_or_default();
        q.exclude = get("exclude")
            .map(|g| vec![urlencoding_decode(&g)])
            .unwrap_or_default();
        q.default_exclude = vec![];
        q.max_files = 200;
        q.max_per_file = 1000;
        let expected = ferro_core::scan::search(&snap, &ws.root, &q, &stop).unwrap();
        let exp_set: BTreeSet<(String, u64)> = expected
            .files
            .iter()
            .flat_map(|f| f.hits.iter().map(|h| (f.path.clone(), h.line as u64)))
            .collect();
        assert_eq!(
            hit_set(&v),
            exp_set,
            "query {i}: {qs} (engine {})",
            v["engine"]
        );
    }
}

fn urlencoding_decode(s: &str) -> String {
    // Decode %XX bytes as UTF-8 properly (+ as space).
    let mut buf = Vec::new();
    let mut it = s.as_bytes().iter().peekable();
    while let Some(&b) = it.next() {
        if b == b'%' {
            let h: Vec<u8> = it.by_ref().take(2).copied().collect();
            if h.len() == 2 {
                if let Ok(hex) = std::str::from_utf8(&h) {
                    if let Ok(n) = u8::from_str_radix(hex, 16) {
                        buf.push(n);
                        continue;
                    }
                }
            }
            buf.push(b'%');
        } else if b == b'+' {
            buf.push(b' ');
        } else {
            buf.push(b);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn freshness_delta_and_tombstone() {
    let (app, st) = state_with_files(&fixture_files());
    build_now(&st);
    let ws = st.ws();
    // Edit: new content must show up immediately (delta-scanned).
    std::fs::write(
        ws.root.join("src/lib.rs"),
        "pub fn serve(port: u16) {}\nquuxorblatz\n",
    )
    .unwrap();
    ws.search.note_changes(&["src/lib.rs".into()], &[]);
    let (s, v) = j(app.clone(), "/api/v1/search?q=quuxorblatz").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["engine"], "index");
    assert!(v["filesMatched"].as_u64().unwrap() >= 1);
    // Delete: tombstoned out of results.
    std::fs::remove_file(ws.root.join("data/words.txt")).unwrap();
    ws.search.note_changes(&[], &["data/words.txt".into()]);
    let (s, v) = j(app.clone(), "/api/v1/search?q=epsilon").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["filesMatched"], 0);
}

#[tokio::test]
async fn policy_auto_skips_small_repos() {
    let (app, st) = state_with_files(&fixture_files());
    // auto (default) with 8 files / bytes: stays off.
    st.ensure_search_built();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        st.ws().search.state(),
        ferro_core::trigram::SearchIndexState::Off
    );
    let (s, v) = j(app, "/api/v1/search?q=serve").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["engine"], "scan");
}

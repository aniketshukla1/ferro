//! Server bootstrap: bind (with port walking), build AppState, merge v1 +
//! static routers, install guard/static/security layers, print the token URL.
//! B1: `ferro-cli` is a thin wrapper over this.

use axum::{
    body::Body,
    http::{header, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    Extension, Router,
};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

use crate::guard::GuardConfig;
use crate::state::{AppState, Host, Limits, Workspace};
use ferro_core::dirs::FerroDirs;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub host: String,
    pub port: u16,
    pub no_git: bool,
    pub narrate: bool,
    pub no_open: bool,
    /// File to open on boot with 1-based line.
    pub initial: Option<(String, usize)>,
    pub token: Option<String>,
    pub allow_hosts: Vec<String>,
    pub no_auth: bool,
    /// Serve static files from disk (frontend iteration). Release embeds.
    pub dev_web: Option<String>,
    pub read_only: bool,
    pub tls: crate::tls::TlsConfig,
    /// SHA-256 fingerprint of the served leaf cert (TLS only).
    pub tls_fingerprint: Option<String>,
    /// Serve under a URL prefix (`/ferro`) for reverse proxies that forward the full path.
    pub base_path: Option<String>,
    /// `--no-lsp`: never start language servers.
    pub no_lsp: bool,
}

/// `--base-path` as `/a/b`: leading slash, no trailing slash; `None` for the root or a path
/// that could smuggle a query, fragment, backslash or dot segment.
pub fn normalize_base_path(raw: &str) -> Option<String> {
    let t = raw.trim().trim_end_matches('/');
    if t.is_empty() {
        return None;
    }
    let t = if t.starts_with('/') {
        t.to_string()
    } else {
        format!("/{t}")
    };
    let ok = !t.contains(['?', '#', '\\', ' '])
        && !t.contains("//")
        && t.split('/').all(|seg| seg != "." && seg != "..");
    ok.then_some(t)
}

/// Serve `app` under `base`: `/base` redirects to `/base/`, `/base/x` reaches the app as `/x`,
/// anything else is 404. The frontend uses relative URLs, so it works unchanged below `/base/`.
pub fn under_base_path(base: String, app: Router) -> Router {
    let base: Arc<str> = Arc::from(base);
    Router::new()
        .fallback_service(app)
        .layer(middleware::from_fn(
            move |mut req: axum::http::Request<axum::body::Body>, next: middleware::Next| {
                let base = base.clone();
                async move {
                    let path = req.uri().path().to_string();
                    let query = req
                        .uri()
                        .query()
                        .map(|q| format!("?{q}"))
                        .unwrap_or_default();
                    if path == *base {
                        return axum::response::Redirect::permanent(&format!("{base}/{query}"))
                            .into_response();
                    }
                    let Some(rest) = path.strip_prefix(&*base).filter(|r| r.starts_with('/'))
                    else {
                        return (axum::http::StatusCode::NOT_FOUND, "not found").into_response();
                    };
                    match format!("{rest}{query}").parse::<axum::http::Uri>() {
                        Ok(uri) => {
                            *req.uri_mut() = uri;
                            next.run(req).await
                        }
                        Err(_) => (axum::http::StatusCode::BAD_REQUEST, "bad path").into_response(),
                    }
                }
            },
        ))
}

pub struct ServerHandle {
    pub port: u16,
    pub token: String,
    pub url: String,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
}

async fn track_activity(
    Extension(last): Extension<Arc<std::sync::Mutex<std::time::Instant>>>,
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> impl IntoResponse {
    if let Ok(mut t) = last.lock() {
        *t = std::time::Instant::now();
    }
    next.run(req).await
}

async fn request_id(
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> impl IntoResponse {
    use std::time::Instant;
    let t0 = Instant::now();
    let id = ulid::Ulid::new().to_string();
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-request-id", id.parse().unwrap());
    h.insert(
        "Server-Timing",
        format!("app;dur={}", t0.elapsed().as_millis())
            .parse()
            .unwrap(),
    );
    res
}

fn panic_envelope(_err: Box<dyn std::any::Any + Send + 'static>) -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .body(Body::from(
            serde_json::json!({ "error": { "code": "internal", "message": "internal error" } })
                .to_string(),
        ))
        .unwrap()
}

fn encode_qs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn build_state(
    root: std::path::PathBuf,
    dirs: FerroDirs,
    host: Host,
    version: String,
) -> Arc<AppState> {
    let ws = Workspace::local(root, &dirs);
    let update = crate::update_check::Updater::new(&version, matches!(host, Host::Cli));
    Arc::new(AppState {
        no_lsp: std::sync::atomic::AtomicBool::new(false),
        ws: arc_swap::ArcSwap::new(ws),
        bus: crate::bus::Events::new(),
        jobs: crate::jobs::JobManager::default(),
        settings: crate::state::SettingsStore { dirs: dirs.clone() },
        ai_convs: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        host,
        dirs,
        limits: Limits::default(),
        version,
        spec_version: "1.11",
        started_at: std::time::Instant::now(),
        update,
        harness_host: parking_lot::Mutex::new(ferro_agent::harness::HarnessHost::default()),
        harness_edits: ferro_agent::harness::ActiveEdits::default(),
        search_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        watch: parking_lot::Mutex::new(None),
    })
}

pub fn build_router(
    state: Arc<AppState>,
    guard: Arc<GuardConfig>,
    last_active: Arc<std::sync::Mutex<std::time::Instant>>,
    _no_git: bool,
    dev_web: Option<String>,
) -> Router {
    let audit = Arc::new(crate::audit::AuditLog::new(&state.dirs.state_dir));
    crate::v1::router()
        .merge(crate::mcp::routes())
        .fallback(crate::legacy::static_file)
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(track_activity))
        .layer(Extension(last_active))
        .layer(middleware::from_fn(request_id))
        .layer(middleware::from_fn(crate::audit::audit_mutations))
        .layer(Extension(audit))
        .layer(middleware::from_fn(crate::read_only::read_only_guard))
        .layer(middleware::from_fn(crate::guard::guards))
        .layer(Extension(guard))
        .layer(Extension(crate::legacy::DevWeb(dev_web)))
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            panic_envelope,
        ))
        .with_state(state)
}

pub async fn serve(
    root: std::path::PathBuf,
    dirs: FerroDirs,
    host_kind: Host,
    version: String,
    o: ServeOpts,
) -> ServerHandle {
    let (listener, bound) = bind_walk(&o.host, o.port).await;
    let state = build_state(root, dirs, host_kind, version);
    serve_with(state, listener, bound, o).await
}

/// Bind with port walking: try the requested port plus the next 100 when taken.
/// Port 0 means "any free port" with no walking.
pub async fn bind_walk(host: &str, want: u16) -> (tokio::net::TcpListener, u16) {
    if want == 0 {
        let l = tokio::net::TcpListener::bind(format!("{host}:0"))
            .await
            .expect("bind");
        let bound = l.local_addr().map(|a| a.port()).unwrap_or(0);
        return (l, bound);
    }
    let mut port = want;
    loop {
        match tokio::net::TcpListener::bind(format!("{host}:{port}")).await {
            Ok(l) => {
                let bound = l.local_addr().map(|a| a.port()).unwrap_or(port);
                return (l, bound);
            }
            Err(_) if port < want.saturating_add(100) => {
                port += 1;
            }
            Err(e) => panic!("bind: {e}"),
        }
    }
}

/// Serve an already-built AppState (PR worktrees built by the caller, which
/// keeps any tempdir alive for the serve lifetime).
pub async fn serve_with(
    state: Arc<AppState>,
    listener: tokio::net::TcpListener,
    bound: u16,
    o: ServeOpts,
) -> ServerHandle {
    let mut guard_cfg = GuardConfig::new(
        o.token.clone(),
        bound,
        o.allow_hosts.clone(),
        o.no_auth,
        o.read_only,
        o.tls.is_https(),
    );
    // "Remember this browser" only where nobody else can reach the server.
    if !o.no_auth && crate::guard::is_loopback_bind(&o.host) {
        match crate::guard::DeviceKey::load_or_create(&state.dirs.state_dir) {
            Ok(k) => guard_cfg = guard_cfg.with_device_key(k),
            Err(e) => tracing::warn!("remembering browsers is off: {e}"),
        }
    }
    state
        .no_lsp
        .store(o.no_lsp, std::sync::atomic::Ordering::Relaxed);
    let base = o.base_path.as_deref().and_then(normalize_base_path);
    let guard = Arc::new(guard_cfg.with_base_path(base.clone()));
    let last_active = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app = build_router(
        state.clone(),
        guard.clone(),
        last_active.clone(),
        o.no_git,
        o.dev_web.clone(),
    );
    let app = match base.clone() {
        Some(b) => under_base_path(b, app),
        None => app,
    };

    // Highlight exact passes announce themselves as `hl` events.
    {
        let bus = state.bus.clone();
        let ws = state.ws();
        ws.hl
            .lock()
            .set_on_exact(Arc::new(move |path: String, mtime_ms: u64| {
                bus.publish(crate::bus::ServerEvent::Hl { path, mtime_ms });
            }));
    }

    // Idle scavenger: after 15 s without requests, trim caches and collect.
    {
        let last = last_active.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tick.tick().await;
                let idle = last.lock().map(|t| t.elapsed()).unwrap_or_default();
                if idle >= std::time::Duration::from_secs(15) {
                    ferro_core::highlight::clear_cache();
                    // Idle trigram catch-up (delta or threshold rebuilds),
                    // off the runtime: the policy reconciles over every path.
                    let s2 = state.clone();
                    tokio::task::spawn_blocking(move || s2.ensure_search_built());
                    state.ensure_symbols_built();
                    // mimalloc keeps freed pages per thread: collect on every thread that did
                    // heavy work (the scan/fuzzy pool and both index build pools), not only here.
                    // SAFETY: mi_collect is thread-safe and only affects the calling thread's heap.
                    let collect = || unsafe { libmimalloc_sys::mi_collect(true) };
                    let ws = state.ws();
                    tokio::task::spawn_blocking(move || {
                        ferro_core::rayon::broadcast(|_| collect());
                        ws.search.broadcast(&collect);
                        ws.symbols.broadcast(&collect);
                        collect();
                    });
                    collect();
                    tracing::debug!("ferro idle {}s: caches trimmed", idle.as_secs());
                }
            }
        });
    }

    // Live updates: fs events, incremental snapshots, status refresh.
    crate::watch::start(&state);

    state.update.set_serve_info(guard.token.clone(), bound);
    crate::update_check::spawn_background(state.clone());

    let host_out = if o.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        o.host.as_str()
    };
    let scheme = if o.tls.is_https() { "https" } else { "http" };
    let prefix = base.as_deref().unwrap_or("");
    let mut url = format!(
        "{scheme}://{}:{}{prefix}/?token={}",
        host_out, bound, guard.token
    );
    if let Some((f, l)) = &o.initial {
        url = format!(
            "{scheme}://{}:{}{prefix}/?token={}&file={}&line={}",
            host_out,
            bound,
            guard.token,
            encode_qs(f),
            l
        );
    }
    if o.narrate {
        tracing::info!("ferro serving {} on {}", state.ws().root.display(), url);
        println!("ferro {}", url);
        if let Some(fp) = &o.tls_fingerprint {
            println!("{}", crate::tls::fingerprint_announcement(fp));
        }
    }
    if o.no_auth {
        tracing::warn!("--no-auth: API auth disabled (loopback only)");
    }
    // Background initial index (walk, then search and symbol policies).
    state.index_in_background(state.ws());
    // Opening is the host's job: ferro-cli opens the browser for Host::Cli,
    // the desktop shell loads the URL in its own window.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let tls = o.tls.clone();
    tokio::spawn(async move {
        match tls {
            crate::tls::TlsConfig::None => {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("serve");
            }
            crate::tls::TlsConfig::Rustls(cfg) => {
                use axum_server::tls_rustls::RustlsConfig;
                let std_listener = listener.into_std().expect("tls listener");
                let rustls = RustlsConfig::from_config(cfg);
                let handle = axum_server::Handle::new();
                let shutdown_handle = handle.clone();
                tokio::spawn(async move {
                    let _ = shutdown_rx.await;
                    shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(2)));
                });
                axum_server::from_tcp_rustls(std_listener, rustls)
                    .expect("tls listener")
                    .handle(handle)
                    .serve(app.into_make_service())
                    .await
                    .expect("serve tls");
            }
        }
    });
    ServerHandle {
        port: bound,
        token: guard.token.clone(),
        url,
        shutdown: shutdown_tx,
    }
}

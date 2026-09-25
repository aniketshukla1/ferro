//! Server bootstrap: bind (with port walking), build AppState, merge v1 +
//! legacy routers, install guard/static/security layers, print the token URL.
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
    Arc::new(AppState {
        ws: arc_swap::ArcSwap::new(ws),
        bus: crate::bus::Events::new(),
        jobs: crate::jobs::JobManager::default(),
        settings: crate::state::SettingsStore { dirs: dirs.clone() },
        host,
        dirs,
        limits: Limits::default(),
        version,
        spec_version: "1.0",
        started_at: std::time::Instant::now(),
    })
}

pub fn build_router(
    state: Arc<AppState>,
    guard: Arc<GuardConfig>,
    last_active: Arc<std::sync::Mutex<std::time::Instant>>,
    no_git: bool,
    dev_web: Option<String>,
) -> Router {
    crate::v1::router()
        .merge(crate::legacy::routes(no_git))
        .fallback(crate::legacy::static_file)
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(track_activity))
        .layer(Extension(last_active))
        .layer(middleware::from_fn(request_id))
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
    let mut guard_cfg = GuardConfig::new(o.token.clone(), bound, o.allow_hosts.clone(), o.no_auth);
    // "Remember this browser" only where nobody else can reach the server.
    if !o.no_auth && crate::guard::is_loopback_bind(&o.host) {
        match crate::guard::DeviceKey::load_or_create(&state.dirs.state_dir) {
            Ok(k) => guard_cfg = guard_cfg.with_device_key(k),
            Err(e) => tracing::warn!("remembering browsers is off: {e}"),
        }
    }
    let guard = Arc::new(guard_cfg);
    let last_active = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app = build_router(
        state.clone(),
        guard.clone(),
        last_active.clone(),
        o.no_git,
        o.dev_web.clone(),
    );

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
        let ws = state.ws();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tick.tick().await;
                let idle = last.lock().map(|t| t.elapsed()).unwrap_or_default();
                if idle >= std::time::Duration::from_secs(15) {
                    ferro_core::highlight::clear_cache();
                    // SAFETY: mi_collect is documented thread-safe; forces a trim.
                    unsafe { libmimalloc_sys::mi_collect(true) };
                    tracing::debug!("ferro idle {}s: caches trimmed", idle.as_secs());
                    let _ = &ws;
                }
            }
        });
    }

    let host_out = if o.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        o.host.as_str()
    };
    let mut url = format!("http://{}:{}/?token={}", host_out, bound, guard.token);
    if let Some((f, l)) = &o.initial {
        url = format!(
            "http://{}:{}/?token={}&file={}&line={}",
            host_out,
            bound,
            guard.token,
            encode_qs(f),
            l
        );
    }
    if o.no_auth {
        tracing::warn!("--no-auth: API auth disabled (loopback only)");
    }
    // Background initial index like before.
    {
        let ws = state.ws();
        let bus = state.bus.clone();
        tokio::spawn(async move {
            ws.index.rebuild().await;
            ws.generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (files, ms) = ws.index.stats();
            bus.publish(crate::bus::ServerEvent::Index {
                state: "ready".into(),
                files,
                ms,
                generation: ws.generation.load(std::sync::atomic::Ordering::Relaxed),
                search_index: "off".into(),
            });
        });
    }
    if o.narrate {
        tracing::info!("ferro serving {} on {}", state.ws().root.display(), url);
        println!("ferro {}", url);
    }
    // Opening is the host's job: ferro-cli opens the browser for Host::Cli,
    // the desktop shell loads the URL in its own window.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("serve");
    });
    ServerHandle {
        port: bound,
        token: guard.token.clone(),
        url,
        shutdown: shutdown_tx,
    }
}

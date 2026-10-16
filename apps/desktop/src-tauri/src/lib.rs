//! Ferro desktop host (B1): runs `ferro-server` in-process on 127.0.0.1:0
//! with a random token, then points the main window at the token URL.
//! No Tauri IPC is reachable from the page (`withGlobalTauri: false`);
//! everything goes over HTTP like the CLI host.
//!
//! `ferro://open?pr=<url>` is registered with the OS bundle. A second launch
//! is handed to the running process. The URL is validated in `deeplink`
//! before it is posted to the local server.

mod deeplink;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::Manager as _;

use ferro_server::{DesktopHost, Host};

struct TauriHost {
    app: tauri::AppHandle,
}

#[async_trait::async_trait]
impl DesktopHost for TauriHost {
    async fn pick_folder(&self) -> Option<PathBuf> {
        use tauri_plugin_dialog::DialogExt;
        self.app
            .dialog()
            .file()
            .set_title("Open folder in Ferro")
            .blocking_pick_folder()
            .map(|p| p.to_string().into())
    }

    async fn open_external(&self, url: &url::Url) -> anyhow::Result<()> {
        use tauri_plugin_opener::OpenerExt;
        self.app
            .opener()
            .open_url(url.as_str(), None::<String>)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    async fn open_window(&self, url: &url::Url) -> anyhow::Result<()> {
        // The window already showing that ferro (same origin and path), else a new one.
        for win in self.app.webview_windows().into_values() {
            if win
                .url()
                .is_ok_and(|u| u.origin() == url.origin() && u.path() == url.path())
            {
                let _ = win.unminimize();
                let _ = win.show();
                return win.set_focus().map_err(|e| anyhow::anyhow!(e.to_string()));
            }
        }
        let label = format!(
            "ferro-{}{}",
            url.port().unwrap_or(0),
            url.path().trim_end_matches('/').replace('/', "-")
        );
        tauri::WebviewWindowBuilder::new(&self.app, label, tauri::WebviewUrl::External(url.clone()))
            .title("Ferro — fast review")
            .inner_size(1280.0, 800.0)
            .build()
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

#[derive(Clone)]
struct ServerEndpoint {
    origin: String,
    token: String,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (server_tx, server_rx) = tokio::sync::watch::channel::<Option<ServerEndpoint>>(None);
    let queue = Arc::new(Mutex::new(Vec::<String>::new()));
    let notify = Arc::new(tokio::sync::Notify::new());

    let mut builder = tauri::Builder::default();
    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Linux and Windows deliver a ferro:// launch as a second process.
            // The deep-link plugin emits the URL to this process; focus it.
            focus_main(app);
        }));
    }

    let queue_for_setup = queue.clone();
    let notify_for_setup = notify.clone();
    builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            register_ferro_scheme(app);
            install_deep_link_listener(app.handle(), &queue_for_setup, &notify_for_setup);
            spawn_deep_link_worker(
                app.handle().clone(),
                server_rx,
                queue_for_setup,
                notify_for_setup,
            );

            // The main window (tauri.conf.json) shows the splash (frontendDist) until the
            // server is up, then loads the token URL.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let dirs = ferro_core::dirs::FerroDirs::resolve();
                let empty = dirs.state_dir.join("Open a project");
                let root = start_root(
                    std::env::current_dir().ok(),
                    &ferro_core::dirs::home_dir(),
                    ferro_server::recent::load(&dirs)
                        .into_iter()
                        .map(|r| PathBuf::from(r.root)),
                    empty.clone(),
                );
                if root == empty {
                    let _ = std::fs::create_dir_all(&empty);
                }
                let host = Host::Desktop(std::sync::Arc::new(TauriHost {
                    app: handle.clone(),
                }));
                let o = ferro_server::server::ServeOpts {
                    host: "127.0.0.1".into(),
                    port: 0,
                    no_git: false,
                    narrate: false,
                    no_open: true,
                    initial: None,
                    token: None,
                    allow_hosts: vec![],
                    no_auth: false,
                    dev_web: None,
                    read_only: false,
                    tls: ferro_server::tls::TlsConfig::None,
                    tls_fingerprint: None,
                    base_path: None,
                    no_lsp: false,
                };
                let h = ferro_server::server::serve(
                    root,
                    dirs,
                    host,
                    env!("CARGO_PKG_VERSION").to_string(),
                    o,
                )
                .await;
                let _ = server_tx.send(Some(ServerEndpoint {
                    origin: format!("http://127.0.0.1:{}", h.port),
                    token: h.token,
                }));
                if let (Ok(url), Ok(win)) = (
                    url::Url::parse(&h.url),
                    handle.get_webview_window("main").ok_or(()),
                ) {
                    let _ = win.navigate(url);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ferro");
}

/// The folder the app opens. Started from a shell, the folder it was started in. Started from
/// Finder or the Dock (`/`) or a Linux launcher (the home folder), no folder was chosen: the most
/// recent project that still exists, else `empty`, a first-run folder whose name ("Open a
/// project") is what the project chip shows until one is picked.
fn start_root(
    cwd: Option<PathBuf>,
    home: &std::path::Path,
    recents: impl IntoIterator<Item = PathBuf>,
    empty: PathBuf,
) -> PathBuf {
    match cwd {
        Some(dir) if dir.parent().is_some() && dir != home => dir,
        _ => recents.into_iter().find(|r| r.is_dir()).unwrap_or(empty),
    }
}

fn register_ferro_scheme(app: &tauri::App) {
    // macOS writes the scheme into Info.plist at `tauri build` from
    // plugins.deep-link.desktop.schemes. Linux and Windows also register the
    // dev (and unpackaged) executable at runtime. A failure here must not
    // stop the app: the bundled registration still applies after install.
    #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        if let Err(err) = app.deep_link().register_all() {
            eprintln!("ferro: could not register the ferro:// scheme for this build: {err}");
        }
    }
    #[cfg(not(any(target_os = "linux", all(debug_assertions, windows))))]
    {
        let _ = app;
    }
}

fn install_deep_link_listener(
    app: &tauri::AppHandle,
    queue: &Arc<Mutex<Vec<String>>>,
    notify: &Arc<tokio::sync::Notify>,
) {
    use tauri_plugin_deep_link::DeepLinkExt;

    let queue_event = queue.clone();
    let notify_event = notify.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            enqueue(&queue_event, &notify_event, url.to_string());
        }
    });
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            enqueue(queue, notify, url.to_string());
        }
    }
    for arg in std::env::args().skip(1) {
        let trimmed = trim_wrapping_quotes(arg.trim());
        if argv_looks_like_ferro_deep_link(trimmed) {
            enqueue(queue, notify, trimmed.to_string());
        }
    }
}

/// True when `trimmed` begins with `ferro://` (any ASCII case). Uses a
/// substring slice so a multi-byte UTF-8 code point at byte 8 cannot panic.
fn argv_looks_like_ferro_deep_link(trimmed: &str) -> bool {
    const PREFIX: &str = "ferro://";
    trimmed
        .get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
}

fn trim_wrapping_quotes(raw: &str) -> &str {
    raw.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(raw)
}

fn enqueue(queue: &Mutex<Vec<String>>, notify: &tokio::sync::Notify, raw: String) {
    let mut pending = queue.lock().unwrap_or_else(|poison| poison.into_inner());
    if pending.iter().any(|queued| queued == &raw) {
        return;
    }
    pending.push(raw);
    drop(pending);
    notify.notify_one();
}

fn spawn_deep_link_worker(
    app: tauri::AppHandle,
    mut server_rx: tokio::sync::watch::Receiver<Option<ServerEndpoint>>,
    queue: Arc<Mutex<Vec<String>>>,
    notify: Arc<tokio::sync::Notify>,
) {
    tauri::async_runtime::spawn(async move {
        // Startup can deliver the same link twice (argv and the open-url
        // event). Collapse that into one open; a later click still works.
        let mut recent: Option<(String, std::time::Instant)> = None;
        loop {
            let batch = {
                let mut pending = queue.lock().unwrap_or_else(|poison| poison.into_inner());
                std::mem::take(&mut *pending)
            };
            if batch.is_empty() {
                notify.notified().await;
                continue;
            }
            for raw in batch {
                focus_main(&app);
                // `already_running` is false on a cold start: this process is
                // the instance the OS just launched, and the server task is
                // still binding. Rejected links return before any request.
                let already_running = server_rx.borrow().is_some();
                match deeplink::plan_open(&raw, already_running) {
                    Err(reject) => show_error(&app, reject.message()),
                    Ok(plan) => {
                        if recent.as_ref().is_some_and(|(url, at)| {
                            url == &plan.canonical_url
                                && at.elapsed() < std::time::Duration::from_secs(2)
                        }) {
                            continue;
                        }
                        match wait_for_server(&mut server_rx).await {
                            None => return,
                            Some(endpoint) => {
                                if let Err(err) = deeplink::post_validated_pr(
                                    &endpoint.origin,
                                    &endpoint.token,
                                    &plan.canonical_url,
                                )
                                .await
                                {
                                    show_error(&app, &err);
                                } else {
                                    recent = Some((plan.canonical_url, std::time::Instant::now()));
                                }
                            }
                        }
                    }
                }
            }
        }
    });
}

async fn wait_for_server(
    server_rx: &mut tokio::sync::watch::Receiver<Option<ServerEndpoint>>,
) -> Option<ServerEndpoint> {
    loop {
        if let Some(endpoint) = server_rx.borrow().clone() {
            return Some(endpoint);
        }
        if server_rx.changed().await.is_err() {
            return None;
        }
    }
}

fn focus_main(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

fn show_error(app: &tauri::AppHandle, message: &str) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    app.dialog()
        .message(message)
        .title("Ferro")
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

#[cfg(test)]
mod start_root_tests {
    use super::start_root;
    use std::path::PathBuf;

    #[test]
    fn a_shell_start_opens_its_folder_and_finder_reopens_the_last_project() {
        let home = PathBuf::from("/Users/ann");
        let empty = PathBuf::from("/state/ferro/Open a project");
        let last = std::env::temp_dir(); // a folder that exists
        let gone = last.join("ferro-no-such-folder");
        let recents = || vec![gone.clone(), last.clone()];
        let project = PathBuf::from("/Users/ann/code/api");
        assert_eq!(
            start_root(Some(project.clone()), &home, recents(), empty.clone()),
            project
        );
        // Finder or the Dock (`/`), a Linux launcher (home), no folder: the last project.
        for cwd in [Some(PathBuf::from("/")), Some(home.clone()), None] {
            assert_eq!(start_root(cwd, &home, recents(), empty.clone()), last);
        }
        // First run: nothing recent that still exists.
        assert_eq!(
            start_root(
                Some(PathBuf::from("/")),
                &home,
                vec![gone.clone()],
                empty.clone()
            ),
            empty
        );
    }
}

#[cfg(test)]
mod argv_scan_tests {
    use super::argv_looks_like_ferro_deep_link;

    #[test]
    fn unicode_argv_after_byte_eight_does_not_panic() {
        assert!(!argv_looks_like_ferro_deep_link("1234567é"));
    }

    #[test]
    fn ferro_scheme_prefix_is_detected_case_insensitively() {
        let link = "ferro://open?pr=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fpull%2F1";
        assert!(argv_looks_like_ferro_deep_link(link));
        assert!(argv_looks_like_ferro_deep_link(&link.to_uppercase()));
    }

    #[test]
    fn non_ferro_argv_is_ignored() {
        assert!(!argv_looks_like_ferro_deep_link("--help"));
        assert!(!argv_looks_like_ferro_deep_link("ferro:"));
    }
}

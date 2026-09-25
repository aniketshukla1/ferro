//! Ferro desktop host (B1): runs `ferro-server` in-process on 127.0.0.1:0
//! with a random token, then points the main window at the token URL.
//! No Tauri IPC is reachable from the page (`withGlobalTauri: false`);
//! everything goes over HTTP like the CLI host.

use std::path::PathBuf;

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
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Splash window first (frontendDist), then swap to the token URL.
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Ferro — fast review")
            .inner_size(1280.0, 800.0)
            .build()?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let dirs = ferro_core::dirs::FerroDirs::resolve();
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
                };
                let h = ferro_server::server::serve(
                    root,
                    dirs,
                    host,
                    env!("CARGO_PKG_VERSION").to_string(),
                    o,
                )
                .await;
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

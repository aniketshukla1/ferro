//! Updates (B8, UPDATE.md): the opt-in daily manifest check, and background updates —
//! download, verify and install a newer release while ferro keeps running, then restart into
//! it when the user says so. No network unless `update.check` / `update.auto` is on (user scope)
//! or the user asks (`POST /update/check`), and never without `FERRO_UPDATE_MANIFEST_URL`.

use ferro_core::dirs::FerroDirs;
use ferro_core::update::{
    apply_release_artifact, artifact_for_target, current_target, daily_check_state_path, is_newer,
    parse_manifest, read_daily_check_state, require_https_url, should_run_daily_check,
    update_check_enabled, write_daily_check_state, DailyCheckState, ReleaseManifest,
    MANIFEST_URL_ENV, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{ApiError, ErrorCode};
use crate::state::{AppState, SettingsStore};

/// `update.check` or `update.auto` is on. User-scoped: a workspace settings file must not open
/// the network.
fn user_update_check_enabled(settings: &SettingsStore) -> bool {
    let raw = settings.raw("", "user");
    update_check_enabled(&raw) || auto_enabled(&raw)
}

fn auto_enabled(values: &std::collections::BTreeMap<String, serde_json::Value>) -> bool {
    values
        .get("update.auto")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// `update.auto` (user scope): install newer releases in the background.
pub fn user_update_auto_enabled(settings: &SettingsStore) -> bool {
    auto_enabled(&settings.raw("", "user"))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `GET /api/v1/update` and the `update` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    /// idle | checking | current | available | downloading | ready | error
    pub state: String,
    pub error: Option<String>,
    pub checked_at: Option<i64>,
    /// `FERRO_UPDATE_MANIFEST_URL` is set.
    pub configured: bool,
    /// This process can install a release and restart into it (the CLI; the desktop app
    /// updates through its own installer).
    pub can_install: bool,
}

pub struct Updater {
    status: parking_lot::Mutex<UpdateStatus>,
    manifest: parking_lot::Mutex<Option<ReleaseManifest>>,
    /// The executable as launched. Captured at startup: once an update replaces the file,
    /// Linux reports the running image's path with a " (deleted)" suffix.
    exe: Option<PathBuf>,
    busy: AtomicBool,
    /// Token and bound port, handed to the restarted process so browsers stay signed in.
    serve: parking_lot::Mutex<Option<(String, u16)>>,
}

impl Updater {
    pub fn new(version: &str, cli: bool) -> Self {
        let exe = if cli {
            std::env::current_exe().ok()
        } else {
            None
        };
        // Debug builds only: start as if an update were installed, to exercise the restart.
        #[cfg(debug_assertions)]
        let debug_ready = cli && std::env::var_os("FERRO_DEBUG_UPDATE_READY").is_some();
        #[cfg(not(debug_assertions))]
        let debug_ready = false;
        Self {
            status: parking_lot::Mutex::new(UpdateStatus {
                current: version.to_string(),
                latest: debug_ready.then(|| format!("{version}+debug")),
                state: if debug_ready { "ready" } else { "idle" }.into(),
                error: None,
                checked_at: None,
                configured: false,
                can_install: exe.is_some(),
            }),
            manifest: parking_lot::Mutex::new(None),
            exe,
            busy: AtomicBool::new(false),
            serve: parking_lot::Mutex::new(None),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        let mut st = self.status.lock().clone();
        st.configured = std::env::var(MANIFEST_URL_ENV).is_ok_and(|u| !u.is_empty());
        st
    }

    pub fn set_serve_info(&self, token: String, port: u16) {
        *self.serve.lock() = Some((token, port));
    }

    fn set(&self, s: &AppState, f: impl FnOnce(&mut UpdateStatus)) {
        f(&mut self.status.lock());
        s.bus.publish(crate::bus::ServerEvent::Update {
            status: self.status(),
        });
    }

    /// A manifest arrived: note whether it is newer than this build.
    fn found(&self, s: &AppState, manifest: ReleaseManifest) {
        let newer = is_newer(&manifest.version, &self.status.lock().current);
        let latest = manifest.version.clone();
        *self.manifest.lock() = Some(manifest);
        self.set(s, |st| {
            // A downloaded update stays ready until the restart.
            if st.state != "ready" {
                st.state = if newer { "available" } else { "current" }.into();
                st.error = None;
            }
            st.latest = Some(latest);
            st.checked_at = Some(now_secs());
        });
    }
}

/// Clears `busy` when a check or install ends, however it ends.
struct Busy<'a>(&'a AtomicBool);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn claim(u: &Updater) -> Option<Busy<'_>> {
    (!u.busy.swap(true, Ordering::AcqRel)).then_some(Busy(&u.busy))
}

/// Hourly: the daily check when the user opted in, then a background install with `update.auto`.
pub fn spawn_background(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if let Err(e) = run_background(&state).await {
                tracing::debug!("update check: {e}");
            }
        }
    });
}

async fn run_background(state: &Arc<AppState>) -> Result<(), String> {
    let url = std::env::var(MANIFEST_URL_ENV).ok();
    let manifest = {
        let Some(_busy) = claim(&state.update) else {
            return Ok(());
        };
        run_daily_check_url(&state.dirs, &state.settings, url.as_deref()).await?
    };
    let Some(manifest) = manifest else {
        return Ok(());
    };
    state.update.found(state, manifest);
    if user_update_auto_enabled(&state.settings) && state.update.status().state == "available" {
        install(state).await?;
    }
    Ok(())
}

/// `POST /update/check`: fetch the manifest now (the user asked), then install in the
/// background when `update.auto` is on.
pub async fn check_now(state: &Arc<AppState>) -> Result<UpdateStatus, ApiError> {
    let url = std::env::var(MANIFEST_URL_ENV)
        .ok()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::Unsupported,
                format!("no update host is configured ({MANIFEST_URL_ENV})"),
            )
        })?;
    require_https_url(&url).map_err(|e| ApiError::new(ErrorCode::BadRequest, e.to_string()))?;
    let Some(busy) = claim(&state.update) else {
        return Ok(state.update.status());
    };
    state.update.set(state, |st| {
        if st.state != "ready" {
            st.state = "checking".into();
        }
    });
    let fetched = fetch_https(&url, MAX_MANIFEST_BYTES, 30)
        .await
        .and_then(|b| parse_manifest(&b).map_err(|e| e.to_string()));
    drop(busy);
    match fetched {
        Ok(manifest) => {
            let _ = write_daily_check_state(
                &daily_check_state_path(&state.dirs.state_dir),
                &DailyCheckState {
                    last_check_unix_secs: Some(now_secs()),
                },
            );
            state.update.found(state, manifest);
        }
        Err(e) => {
            state.update.set(state, |st| {
                st.state = "error".into();
                st.error = Some(e.clone());
            });
            return Err(ApiError::new(ErrorCode::Upstream, e));
        }
    }
    if user_update_auto_enabled(&state.settings) && state.update.status().state == "available" {
        let s2 = state.clone();
        tokio::spawn(async move {
            let _ = install(&s2).await;
        });
    }
    Ok(state.update.status())
}

/// Download, verify and install the latest release over the running executable. The process
/// keeps running the old image until it restarts.
pub async fn install(state: &Arc<AppState>) -> Result<(), String> {
    let u = &state.update;
    let exe = u
        .exe
        .clone()
        .ok_or("this ferro cannot update itself (desktop app)")?;
    let target = current_target();
    let (version, art) = {
        let m = u.manifest.lock();
        let m = m.as_ref().ok_or("no release found yet: check first")?;
        let art = artifact_for_target(m, &target).map_err(|e| e.to_string())?;
        (m.version.clone(), art.clone())
    };
    if !is_newer(&version, &u.status.lock().current) {
        return Err(format!("{version} is not newer than this build"));
    }
    let Some(_busy) = claim(u) else {
        return Ok(());
    };
    u.set(state, |st| {
        st.state = "downloading".into();
        st.error = None;
    });
    let res = async {
        let bytes = fetch_https(&art.url, MAX_ARCHIVE_BYTES, 300).await?;
        let t2 = target.clone();
        tokio::task::spawn_blocking(move || {
            apply_release_artifact(&exe, &t2, &bytes, &art.sha256).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
    .await;
    match res {
        Ok(()) => {
            tracing::info!("update: installed {version}; restart to run it");
            u.set(state, |st| {
                st.state = "ready".into();
                st.latest = Some(version);
            });
            Ok(())
        }
        Err(e) => {
            u.set(state, |st| {
                st.state = "error".into();
                st.error = Some(e.clone());
            });
            Err(e)
        }
    }
}

/// `POST /update/restart`: re-run the installed binary with the same arguments, port and token
/// (so open browsers reconnect signed in). Refused while an agent job runs.
pub fn restart(state: &Arc<AppState>) -> Result<(), ApiError> {
    let u = &state.update;
    let exe = u.exe.clone().ok_or_else(|| {
        ApiError::new(
            ErrorCode::Unsupported,
            "this ferro cannot restart itself (desktop app)",
        )
    })?;
    if u.status.lock().state != "ready" {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "no installed update is waiting for a restart",
        ));
    }
    if state.jobs.list().iter().any(|j| {
        matches!(
            j.state,
            crate::jobs::JobState::Running | crate::jobs::JobState::Queued
        )
    }) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "a job is still running; restart when it finishes",
        ));
    }
    let (token, port) = u
        .serve
        .lock()
        .clone()
        .ok_or_else(|| ApiError::new(ErrorCode::NotReady, "server is still starting"))?;
    let file = state
        .dirs
        .state_dir
        .join(format!("restart-{}.json", std::process::id()));
    write_private(
        &file,
        serde_json::json!({ "token": token, "port": port })
            .to_string()
            .as_bytes(),
    )
    .map_err(|e| ApiError::new(ErrorCode::Internal, format!("restart state: {e}")))?;
    let args = restart_args(std::env::args_os().skip(1), &file);
    // Let the 202 reach the browser first.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        tracing::info!("update: restarting into {}", exe.display());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Sockets and pipes are close-on-exec: the port frees and language servers see EOF.
            let err = std::process::Command::new(&exe).args(&args).exec();
            tracing::error!("update: restart failed: {err}");
        }
        #[cfg(not(unix))]
        {
            match std::process::Command::new(&exe).args(&args).spawn() {
                Ok(_) => std::process::exit(0),
                Err(e) => tracing::error!("update: restart failed: {e}"),
            }
        }
    });
    Ok(())
}

/// The arguments to restart with: the originals minus an older `--restart-state`, plus
/// `--no-open` (the browser is already open) and the new state file.
pub fn restart_args(
    original: impl Iterator<Item = std::ffi::OsString>,
    state_file: &Path,
) -> Vec<std::ffi::OsString> {
    let mut out = Vec::new();
    let mut it = original;
    while let Some(a) = it.next() {
        if a == "--restart-state" {
            it.next();
            continue;
        }
        if a.to_string_lossy().starts_with("--restart-state=") {
            continue;
        }
        out.push(a);
    }
    if !out.iter().any(|a| a == "--no-open") {
        out.push("--no-open".into());
    }
    out.push("--restart-state".into());
    out.push(state_file.as_os_str().to_owned());
    out
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(bytes)
}

/// Read and delete a restart state file (the CLI's `--restart-state`): `(token, port)`.
pub fn take_restart_state(path: &Path) -> Option<(String, u16)> {
    let text = std::fs::read_to_string(path).ok();
    let _ = std::fs::remove_file(path);
    let v: serde_json::Value = serde_json::from_str(&text?).ok()?;
    Some((
        v["token"].as_str()?.to_string(),
        u16::try_from(v["port"].as_u64()?).ok()?,
    ))
}

/// The daily manifest fetch when the user opted in and a day has passed. `Ok(None)` when it
/// did not run.
async fn run_daily_check_url(
    dirs: &FerroDirs,
    settings: &SettingsStore,
    manifest_url: Option<&str>,
) -> Result<Option<ReleaseManifest>, String> {
    if !user_update_check_enabled(settings) {
        return Ok(None);
    }
    let now = now_secs();
    let state_path = daily_check_state_path(&dirs.state_dir);
    let state = read_daily_check_state(&state_path);
    if !should_run_daily_check(true, state.last_check_unix_secs, now) {
        return Ok(None);
    }
    let url = manifest_url.ok_or_else(|| {
        format!("{MANIFEST_URL_ENV} unset; board has not chosen an update host yet")
    })?;
    // Scheme check before any socket is opened.
    require_https_url(url).map_err(|e| e.to_string())?;
    let bytes = fetch_https(url, MAX_MANIFEST_BYTES, 30).await?;
    let manifest = parse_manifest(&bytes).map_err(|e| e.to_string())?;
    tracing::info!(
        "update check: latest release {} ({} artifacts)",
        manifest.version,
        manifest.artifacts.len()
    );
    let next = DailyCheckState {
        last_check_unix_secs: Some(now),
    };
    write_daily_check_state(&state_path, &next).map_err(|e| e.to_string())?;
    Ok(Some(manifest))
}

/// HTTPS GET with a hard body cap. Redirects that leave HTTPS are refused.
/// Error text has query strings stripped so signed URLs are not logged.
pub async fn fetch_https(
    url: &str,
    max_bytes: usize,
    timeout_secs: u64,
) -> Result<Vec<u8>, String> {
    require_https_url(url).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error(std::io::Error::other("too many redirects"));
            }
            match require_https_url(attempt.url().as_str()) {
                Ok(()) => attempt.follow(),
                Err(_) => attempt.error(std::io::Error::other("redirect left https")),
            }
        }))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| net_err("update request", &e))?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| net_err("update request", &e))?
        .error_for_status()
        .map_err(|e| net_err("update request", &e))?;
    if resp.content_length().is_some_and(|n| n > max_bytes as u64) {
        return Err(format!("download exceeds {max_bytes} bytes"));
    }
    let mut out = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if out.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(format!("download exceeds {max_bytes} bytes"));
                }
                out.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return Err(net_err("update download", &e)),
        }
    }
    Ok(out)
}

fn net_err(ctx: &str, err: &impl std::fmt::Display) -> String {
    format!("{ctx}: {}", strip_query(&err.to_string()))
}

fn strip_query(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '?' {
            out.push('?');
            out.push('…');
            while chars.peek().is_some_and(|n| !n.is_whitespace()) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Test hook: returns whether a network fetch would run (no I/O).
pub fn would_fetch_network(
    settings: &SettingsStore,
    state: &DailyCheckState,
    now_unix_secs: i64,
    manifest_url_set: bool,
) -> bool {
    if !user_update_check_enabled(settings) {
        return false;
    }
    if !manifest_url_set {
        return false;
    }
    should_run_daily_check(true, state.last_check_unix_secs, now_unix_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferro_core::dirs::FerroDirs;
    use std::time::Duration;
    use tempfile::tempdir;

    fn dirs_and_settings(check: bool) -> (tempfile::TempDir, FerroDirs, SettingsStore) {
        let dir = tempdir().unwrap();
        let dirs = FerroDirs::new(
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("h"),
        );
        if check {
            std::fs::create_dir_all(&dirs.config_dir).unwrap();
            std::fs::write(
                dirs.config_dir.join("settings.json"),
                r#"{"update.check":true}"#,
            )
            .unwrap();
        }
        let settings = SettingsStore { dirs: dirs.clone() };
        (dir, dirs, settings)
    }

    #[test]
    fn restart_args_keep_the_originals_once() {
        let f = std::path::Path::new("/s/restart-1.json");
        let a: Vec<std::ffi::OsString> = ["/repo", "-p", "7800", "--restart-state", "/s/old.json"]
            .iter()
            .map(Into::into)
            .collect();
        let out = restart_args(a.into_iter(), f);
        assert_eq!(
            out,
            [
                "/repo",
                "-p",
                "7800",
                "--no-open",
                "--restart-state",
                "/s/restart-1.json"
            ]
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn restart_state_is_read_once() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("restart.json");
        write_private(&p, br#"{"token":"t0k","port":7801}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(take_restart_state(&p), Some(("t0k".into(), 7801)));
        assert!(!p.exists());
        assert_eq!(take_restart_state(&p), None);
    }

    #[test]
    fn predicate_stays_off_when_setting_is_off() {
        let (_dir, _dirs, settings) = dirs_and_settings(false);
        assert!(!would_fetch_network(
            &settings,
            &DailyCheckState::default(),
            99_999,
            true
        ));
    }

    #[tokio::test]
    async fn disabled_check_opens_no_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // https so a missing scheme check cannot hide a setting-gate bug:
        // an enabled check would open TCP before the TLS handshake fails.
        let url = format!("https://127.0.0.1:{port}/manifest.json");
        let (_dir, dirs, settings) = dirs_and_settings(false);
        let accept = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(400), listener.accept()).await
        });
        run_daily_check_url(&dirs, &settings, Some(&url))
            .await
            .unwrap();
        let accepted = accept.await.unwrap();
        assert!(
            accepted.is_err(),
            "update.check is off but a connection was accepted"
        );
    }

    #[tokio::test]
    async fn http_manifest_is_refused_before_connect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/manifest.json");
        let (_dir, dirs, settings) = dirs_and_settings(true);
        let accept = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(400), listener.accept()).await
        });
        let err = run_daily_check_url(&dirs, &settings, Some(&url))
            .await
            .unwrap_err();
        assert!(err.contains("https"), "{err}");
        let accepted = accept.await.unwrap();
        assert!(accepted.is_err(), "http manifest URL opened a socket");
    }

    #[tokio::test]
    async fn workspace_setting_does_not_open_a_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("https://127.0.0.1:{port}/manifest.json");
        let (_dir, dirs, settings) = dirs_and_settings(false);
        let ws = dirs.workspace_state_dir("");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("settings.json"), r#"{"update.check":true}"#).unwrap();
        let accept = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(400), listener.accept()).await
        });
        run_daily_check_url(&dirs, &settings, Some(&url))
            .await
            .unwrap();
        let accepted = accept.await.unwrap();
        assert!(accepted.is_err(), "workspace update.check opened a socket");
    }
}

//! AppState and Workspace per BACKEND.md § 4.2.
//! Handlers load the workspace once per request and use that Arc throughout.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::bus::Events;
use crate::jobs::JobManager;
use ferro_core::dirs::FerroDirs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Pr,
}

/// Live PR session (B4). Held by `Workspace.pr` when in PR mode.
pub struct PrSession {
    pub pr_ref: ferro_forge::ForgeRef,
    pub meta: parking_lot::RwLock<ferro_forge::github::PullMeta>,
    pub client: std::sync::Arc<ferro_forge::ForgeClient>,
    /// Memory only, never serialized or logged (refresh re-opens worktrees).
    pub token: Option<String>,
    pub token_source: Option<ferro_forge::TokenSource>,
    pub worktree: parking_lot::RwLock<ferro_forge::OpenedPr>,
    pub store: ferro_forge::store::ReviewStore,
    pub checks: parking_lot::RwLock<Option<ferro_forge::github::Checks>>,
    pub can_push: parking_lot::RwLock<Option<bool>>,
    pub can_push_known: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub enum Host {
    Cli,
    Desktop(Arc<dyn DesktopHost>),
}

#[async_trait::async_trait]
pub trait DesktopHost: Send + Sync {
    async fn pick_folder(&self) -> Option<PathBuf>;
    async fn open_external(&self, url: &url::Url) -> anyhow::Result<()>;
}

/// See BACKEND.md §1.7.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_window_lines: usize,
    pub max_cols: usize,
    pub max_raw_bytes: u64,
    pub max_markdown_bytes: u64,
    pub max_search_files: usize,
    pub max_diff_rows: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_window_lines: 1000,
            max_cols: 4000,
            max_raw_bytes: 32 * 1024 * 1024,
            max_markdown_bytes: 4 * 1024 * 1024,
            max_search_files: 1000,
            max_diff_rows: 20000,
        }
    }
}

#[derive(Debug)]
pub struct GitRepo {
    pub root: PathBuf,
    pub repo: ferro_core::git::GitRepo,
}

pub struct Workspace {
    pub key: String,
    pub root: PathBuf,
    pub mode: Mode,
    /// Legacy file index (B2 swaps in FileIndex). Generation bumps on rebuild.
    pub index: Arc<ferro_core::Index>,
    pub generation: AtomicU64,
    pub lines: crate::lines::LineIndex,
    pub hl: Arc<parking_lot::Mutex<crate::hl::Highlighter>>,
    pub git: Option<GitRepo>,
    /// Live PR session in PR mode (B4).
    pub pr: Option<Arc<PrSession>>,
    /// Trigram search engine (B2b): background-built, mmap-queried.
    pub search: Arc<ferro_core::trigram::SearchEngine>,
    /// Workspace symbol index (B6): background-built, persisted.
    pub symbols: Arc<ferro_core::symindex::SymbolIndex>,
    /// Latest status payload (B3 watcher + mutations); tree reads it for
    /// `git`/`dirty` without spawning git per request.
    pub git_status: parking_lot::RwLock<Option<ferro_core::git::GitStatus>>,
    pub review: ferro_agent::ReviewStore,
    pub session_path: PathBuf,
    /// Language servers for diagnostics, created on first use (`AppState::lsp`).
    pub lsp: std::sync::OnceLock<Arc<crate::lsp::LspManager>>,
}

impl Workspace {
    fn build(root: PathBuf, dirs: &FerroDirs, mode: Mode, pr: Option<Arc<PrSession>>) -> Arc<Self> {
        let index = Arc::new(ferro_core::Index::new(root.clone()));
        let key = dirs.workspace_key(index.root());
        let session_path = dirs.workspace_state_dir(&key).join("session.json");
        let git = is_repo(index.root()).then(|| {
            let mut repo = ferro_core::git::GitRepo::new(index.root().to_path_buf());
            // A PR checkout is untrusted code: every git command on it runs
            // hardened (no hooks, fsmonitor, submodule recursion or file
            // transport), or a repo with an in-tree `core.hooksPath` would
            // run the PR's own hooks on commit or push.
            if matches!(mode, Mode::Pr) {
                repo = repo.with_env(ferro_forge::checkout::untrusted_env(&[], false));
            }
            GitRepo {
                root: index.root().to_path_buf(),
                repo,
            }
        });
        let search =
            ferro_core::trigram::SearchEngine::new(dirs.workspace_cache_dir(&key).join("trigram"));
        let symbols = ferro_core::symindex::SymbolIndex::new(
            dirs.workspace_cache_dir(&key),
            index.root().to_path_buf(),
        );
        symbols.preload(&index.file_index.load());
        Arc::new(Self {
            key,
            root: index.root().to_path_buf(),
            mode,
            index,
            generation: AtomicU64::new(0),
            lines: crate::lines::LineIndex::new(),
            hl: Arc::new(parking_lot::Mutex::new(crate::hl::Highlighter::new())),
            git,
            pr,
            search,
            symbols,
            git_status: parking_lot::RwLock::new(None),
            review: ferro_agent::ReviewStore::default(),
            session_path,
            lsp: std::sync::OnceLock::new(),
        })
    }

    pub fn local(root: PathBuf, dirs: &FerroDirs) -> Arc<Self> {
        Self::build(root, dirs, Mode::Local, None)
    }

    /// PR-mode workspace rooted at an opened worktree (git runs hardened,
    /// see [`Workspace::build`]).
    pub fn pr(root: PathBuf, session: Arc<PrSession>, dirs: &FerroDirs) -> Arc<Self> {
        Self::build(root, dirs, Mode::Pr, Some(session))
    }
}

fn is_repo(root: &std::path::Path) -> bool {
    root.join(".git").exists()
}

pub struct AppState {
    /// `--no-lsp`: never start language servers.
    pub no_lsp: std::sync::atomic::AtomicBool,
    pub ws: arc_swap::ArcSwap<Workspace>,
    pub bus: Events,
    pub jobs: JobManager,
    pub settings: SettingsStore,
    /// AI conversations by `c_…` id (B5): 1 h TTL, 20 turns. Evicted lazily
    /// on each `/ai/ask`.
    pub ai_convs:
        Arc<parking_lot::Mutex<std::collections::HashMap<String, ferro_agent::Conversation>>>,
    pub host: Host,
    pub dirs: FerroDirs,
    pub limits: Limits,
    pub version: String,
    pub spec_version: &'static str,
    pub started_at: std::time::Instant,
    /// Update check / background install / restart (API.md § 13).
    pub update: crate::update_check::Updater,
    /// Harness lookup and the production 10-minute dispatch cap (B7).
    pub harness_host: parking_lot::Mutex<ferro_agent::harness::HarnessHost>,
    /// In-flight `harness.edit` ranges. Overlap on the same path is 409.
    pub harness_edits: ferro_agent::harness::ActiveEdits,
    /// At most 2 concurrent full scans (§ 4.3); extra ones wait.
    pub search_slots: Arc<tokio::sync::Semaphore>,
    /// Live watcher handle (B3); None in tests or when watching failed
    /// (adaptive polling fallback runs instead).
    pub watch: parking_lot::Mutex<Option<ferro_core::watch::WatchHandle>>,
}

impl AppState {
    pub fn ws(&self) -> Arc<Workspace> {
        self.ws.load_full()
    }

    /// The current workspace's language servers. `lsp.enabled`: `auto` (default) runs them for
    /// local folders but not PR checkouts (untrusted code), `on` always, `off` never.
    pub fn lsp(&self) -> Arc<crate::lsp::LspManager> {
        let ws = self.ws();
        ws.lsp
            .get_or_init(|| {
                let eff = self.settings.effective(&ws.key);
                let setting = eff
                    .get("lsp.enabled")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto")
                    .to_string();
                let enabled = !self.no_lsp.load(std::sync::atomic::Ordering::Relaxed)
                    && match setting.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => !matches!(ws.mode, Mode::Pr),
                    };
                Arc::new(crate::lsp::LspManager::new(
                    ws.root.clone(),
                    enabled,
                    self.bus.clone(),
                ))
            })
            .clone()
    }

    /// Policy-check the trigram index (builds in the background when due).
    /// Cheap when fresh; called after index rebuilds, on watcher batches,
    /// and on idle. Search excludes and size limits apply at query time,
    /// so the index itself never depends on them.
    pub fn ensure_search_built(&self) {
        let ws = self.ws();
        let mode = self
            .settings
            .effective(&ws.key)
            .get("search.index")
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_string();
        ws.search
            .ensure_built(&ws.index.file_index.load(), &ws.root, &mode);
    }

    /// Index a workspace that just became current (boot, folder open, PR
    /// open) in the background: walk, then the search-index policy (off the
    /// async runtime: it reconciles over every path), then an `index` event
    /// carrying the real search-index state.
    pub fn index_in_background(self: &Arc<Self>, ws: Arc<Workspace>) {
        self.wire_search_events(&ws);
        let s = self.clone();
        tokio::spawn(async move {
            ws.index.rebuild().await;
            ws.generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let s2 = s.clone();
            let _ = tokio::task::spawn_blocking(move || s2.ensure_search_built()).await;
            s.ensure_symbols_built();
            let (files, ms) = ws.index.stats();
            s.bus.publish(crate::bus::ServerEvent::Index {
                state: "ready".into(),
                files,
                ms,
                generation: ws.generation.load(std::sync::atomic::Ordering::Relaxed),
                search_index: ws.search.state().as_str().into(),
            });
        });
    }

    /// Wire a workspace's search index into live updates: its state
    /// transitions go out as `index` events. Every workspace that becomes
    /// current (boot, folder open, PR open) goes through here.
    pub fn wire_search_events(&self, ws: &Arc<Workspace>) {
        let bus = self.bus.clone();
        let weak = Arc::downgrade(ws);
        ws.search.set_on_transition(Arc::new(move || {
            let Some(ws) = weak.upgrade() else {
                return;
            };
            let (files, ms) = ws.index.stats();
            bus.publish(crate::bus::ServerEvent::Index {
                state: "ready".into(),
                files,
                ms,
                generation: ws.generation.load(std::sync::atomic::Ordering::Relaxed),
                search_index: ws.search.state().as_str().into(),
            });
        }));
    }

    /// Rebuild the symbol index in the background when the snapshot moved
    /// on. Cheap when fresh; call alongside `ensure_search_built`.
    /// Background refresh: a no-op until something has used symbols (see `SymbolIndex::refresh`).
    pub fn ensure_symbols_built(&self) {
        let ws = self.ws();
        ws.symbols.refresh(&ws.index.file_index.load());
    }
}

/// Settings store: defaults < user < workspace. Unknown `ui.*` passthrough.
/// The workspace key is passed per call so workspace switches stay correct.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    pub dirs: FerroDirs,
}

impl SettingsStore {
    pub fn schema() -> Vec<serde_json::Value> {
        serde_json::from_value(serde_json::json!([
            {"key":"files.exclude","section":"files","title":"Excluded globs","description":"Extra ignore globs for the file index.","type":"string[]","default":[],"scopes":["user","workspace"]},
            {"key":"theme","section":"ui","title":"Theme","description":"Color theme.","type":"enum","default":"forge","enum":["forge","paper","mocha","nord","dracula","gruvbox","tokyo"],"scopes":["user","workspace"]},
            {"key":"wordWrap","section":"ui","title":"Word wrap","description":"Soft-wrap long lines.","type":"bool","default":false,"scopes":["user","workspace"]},
            {"key":"sidebar","section":"ui","title":"Sidebar","description":"Show the file tree.","type":"bool","default":true,"scopes":["user","workspace"]},
            {"key":"askMaxSteps","section":"ai","title":"Ask max steps","description":"Agent loop step cap.","type":"int","default":8,"min":1,"max":16,"scopes":["user","workspace"]},
            {"key":"search.exclude","section":"search","title":"Search excludes","description":"Extra globs excluded from search.","type":"string[]","default":["**/vendor/**"],"scopes":["user","workspace"]},
            {"key":"search.maxFileBytes","section":"search","title":"Max searched file size","description":"Files larger than this are skipped.","type":"int","default":8388608,"min":1024,"scopes":["user","workspace"]},
            {"key":"search.index","section":"search","title":"Trigram index","description":"Background trigram index for instant search.","type":"enum","default":"auto","enum":["auto","on","off"],"scopes":["user","workspace"]},
            {"key":"git.autoRefresh","section":"git","title":"Auto refresh","description":"Refresh git status on filesystem changes.","type":"bool","default":true,"scopes":["user","workspace"]},
            {"key":"review.defaultEvent","section":"review","title":"Default review event","description":"Submit action used by default.","type":"enum","default":"COMMENT","enum":["COMMENT","APPROVE","REQUEST_CHANGES"],"scopes":["user","workspace"]},
            {"key":"ai.provider","section":"ai","title":"Provider","description":"LLM provider selection.","type":"enum","default":"auto","enum":["auto","anthropic","openai","gemini","ollama","openai-compatible","off"],"scopes":["user","workspace"]},
            {"key":"ai.model","section":"ai","title":"Model","description":"Model override (empty = provider default).","type":"string","default":"","scopes":["user","workspace"]},
            {"key":"ai.baseUrl","section":"ai","title":"Base URL","description":"OpenAI-compatible endpoint override.","type":"string","default":"","scopes":["user","workspace"]},
            {"key":"ai.effort.ask","section":"ai","title":"Ask effort","description":"Effort for ask.","type":"enum","default":"high","enum":["low","medium","high","xhigh","max"],"scopes":["user","workspace"]},
            {"key":"ai.effort.review","section":"ai","title":"Review effort","description":"Effort for AI review.","type":"enum","default":"high","enum":["low","medium","high","xhigh","max"],"scopes":["user","workspace"]},
            {"key":"ai.effort.commit","section":"ai","title":"Commit effort","description":"Effort for commit messages.","type":"enum","default":"low","enum":["low","medium","high","xhigh","max"],"scopes":["user","workspace"]},
            {"key":"ai.maxSteps","section":"ai","title":"Max steps","description":"Agent loop cap.","type":"int","default":12,"min":1,"max":32,"scopes":["user","workspace"]},
            {"key":"ai.redactSecrets","section":"ai","title":"Redact secrets","description":"Strip secrets before sending.","type":"bool","default":true,"scopes":["user","workspace"]},
            {"key":"ai.neverSend","section":"ai","title":"Never send","description":"Globs never sent to any provider.","type":"string[]","default":[".env*","**/*.pem","**/*.key","**/id_rsa*","**/*.p12","**/secrets/**"],"scopes":["user","workspace"]},
            {"key":"lsp.enabled","section":"editor","title":"Language servers","description":"Diagnostics in the Problems tab from language servers found on PATH (rust-analyzer, gopls, typescript-language-server, pyright/pylsp, clangd). auto: local folders only, never PR checkouts.","type":"enum","default":"auto","enum":["auto","on","off"],"scopes":["user","workspace"]},
            {"key":"checks.runTests","section":"checks","title":"Run tests","description":"Let the Checks tab run the project's tests for a change (always on your click, after showing the command). auto: local folders only, never PR checkouts, whose code is untrusted.","type":"enum","default":"auto","enum":["auto","on","off"],"scopes":["user","workspace"]},
            {"key":"harness.selected","section":"harness","title":"Harness","description":"Selected external harness (B7). Empty until PUT /harness.","type":"string","default":"","scopes":["user","workspace"]},
            {"key":"harness.model","section":"harness","title":"Harness model","description":"Model passed to the selected harness. Empty uses the harness default.","type":"string","default":"","scopes":["user","workspace"]},
            {"key":"update.check","section":"updates","title":"Update check","description":"Check once a day for a new release (needs an update host).","type":"bool","default":false,"scopes":["user"]},
            {"key":"update.auto","section":"updates","title":"Update in the background","description":"Download and install new releases while ferro runs; restart when it suits you.","type":"bool","default":false,"scopes":["user"]}
        ])).unwrap()
    }

    fn defaults() -> std::collections::BTreeMap<String, serde_json::Value> {
        Self::schema()
            .into_iter()
            .filter_map(|d| {
                let k = d.get("key")?.as_str()?.to_string();
                Some((k, d.get("default")?.clone()))
            })
            .collect()
    }

    fn read_file(path: &std::path::Path) -> std::collections::BTreeMap<String, serde_json::Value> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Default::default();
        };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(m)) => m.into_iter().collect(),
            _ => Default::default(),
        }
    }

    fn user_path(&self) -> PathBuf {
        self.dirs.config_dir.join("settings.json")
    }

    fn ws_path(&self, key: &str) -> PathBuf {
        self.dirs.workspace_state_dir(key).join("settings.json")
    }

    /// Raw stored values for one scope (no defaults merged).
    pub fn raw(
        &self,
        key: &str,
        scope: &str,
    ) -> std::collections::BTreeMap<String, serde_json::Value> {
        match scope {
            "user" => Self::read_file(&self.user_path()),
            _ => Self::read_file(&self.ws_path(key)),
        }
    }

    /// Effective values: defaults < user < workspace.
    pub fn effective(&self, key: &str) -> std::collections::BTreeMap<String, serde_json::Value> {
        let mut out = Self::defaults();
        for (k, v) in Self::read_file(&self.user_path()) {
            out.insert(k, v);
        }
        for (k, v) in Self::read_file(&self.ws_path(key)) {
            if Self::scope_allows(&k, "workspace") {
                out.insert(k, v);
            }
        }
        out
    }

    /// Merge a patch into a scope file. `None` values reset the key.
    /// `ui.*` keys pass through unvalidated (≤ 64 KiB total).
    pub fn save(
        &self,
        key: &str,
        scope: &str,
        patch: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, String> {
        let path = match scope {
            "user" => self.user_path(),
            "workspace" => self.ws_path(key),
            _ => return Err(format!("unknown scope: {scope}")),
        };
        let mut cur = Self::read_file(&path);
        for (k, v) in &patch {
            if !Self::scope_allows(k, scope) {
                return Err(format!("{k} cannot be set at scope {scope}"));
            }
            if v.is_null() {
                cur.remove(k);
            } else {
                Self::validate(k, v)?;
                cur.insert(k.clone(), v.clone());
            }
        }
        let ui_bytes: usize = cur
            .iter()
            .filter(|(k, _)| k.starts_with("ui."))
            .map(|(k, v)| k.len() + v.to_string().len())
            .sum();
        if ui_bytes > 64 * 1024 {
            return Err("ui.* settings exceed 64 KiB".into());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let obj: serde_json::Map<String, serde_json::Value> = cur.into_iter().collect();
        ferro_core::settings::atomic_write(
            &path,
            serde_json::to_string_pretty(&obj)
                .unwrap_or_default()
                .as_bytes(),
        )?;
        Ok(self.effective(key))
    }

    fn scope_allows(key: &str, scope: &str) -> bool {
        if key.starts_with("ui.") {
            return scope == "user" || scope == "workspace";
        }
        Self::schema().into_iter().any(|d| {
            d.get("key").and_then(|k| k.as_str()) == Some(key)
                && d.get("scopes")
                    .and_then(|s| s.as_array())
                    .is_some_and(|scopes| scopes.iter().any(|s| s.as_str() == Some(scope)))
        })
    }

    fn validate(key: &str, v: &serde_json::Value) -> Result<(), String> {
        if key.starts_with("ui.") {
            return Ok(());
        }
        let def = Self::schema()
            .into_iter()
            .find(|d| d.get("key").and_then(|k| k.as_str()) == Some(key));
        let Some(def) = def else {
            return Err(format!("unknown setting: {key}"));
        };
        let ty = def.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let ok = match ty {
            "bool" => v.is_boolean(),
            "int" => v.as_i64().is_some_and(|n| {
                let min = def.get("min").and_then(|m| m.as_i64()).unwrap_or(i64::MIN);
                let max = def.get("max").and_then(|m| m.as_i64()).unwrap_or(i64::MAX);
                n >= min && n <= max
            }),
            "string" => v.is_string(),
            "string[]" => v
                .as_array()
                .is_some_and(|a| a.iter().all(|x| x.is_string())),
            "enum" => v.as_str().is_some_and(|s| {
                def.get("enum")
                    .and_then(|e| e.as_array())
                    .map(|arr| arr.iter().any(|x| x.as_str() == Some(s)))
                    .unwrap_or(false)
            }),
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(format!("invalid value for {key}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_merge_and_validate() {
        let t = tempfile::tempdir().unwrap();
        let dirs = FerroDirs::new(t.path().join("c"), t.path().join("s"), t.path().join("h"));
        let st = SettingsStore { dirs };
        assert_eq!(st.effective("k1")["theme"], serde_json::json!("forge"));
        st.save(
            "k1",
            "user",
            [("theme".into(), serde_json::json!("paper"))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        st.save(
            "k1",
            "workspace",
            [("sidebar".into(), serde_json::json!(false))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        let eff = st.effective("k1");
        assert_eq!(eff["theme"], serde_json::json!("paper"));
        assert_eq!(eff["sidebar"], serde_json::json!(false));
        assert!(st
            .save(
                "k1",
                "workspace",
                [("theme".into(), serde_json::json!("nope"))]
                    .into_iter()
                    .collect()
            )
            .is_err());
        assert!(st.save("k1", "nope", Default::default()).is_err());
        // null resets to default
        st.save(
            "k1",
            "user",
            [("theme".into(), serde_json::Value::Null)]
                .into_iter()
                .collect(),
        )
        .unwrap();
        assert_eq!(st.effective("k1")["theme"], serde_json::json!("forge"));
        // ui.* passthrough
        st.save(
            "k1",
            "user",
            [("ui.layout".into(), serde_json::json!({"a": 1}))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        assert_eq!(st.effective("k1")["ui.layout"], serde_json::json!({"a": 1}));
        assert!(st
            .save(
                "k1",
                "workspace",
                [("update.check".into(), serde_json::json!(true))]
                    .into_iter()
                    .collect(),
            )
            .is_err());
        // A hand-written workspace file cannot turn the daily check on.
        let ws = st.dirs.workspace_state_dir("k1");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("settings.json"), r#"{"update.check":true}"#).unwrap();
        assert_eq!(st.effective("k1")["update.check"], serde_json::json!(false));
        st.save(
            "k1",
            "user",
            [("update.check".into(), serde_json::json!(true))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        assert_eq!(st.raw("", "user")["update.check"], serde_json::json!(true));
    }
}

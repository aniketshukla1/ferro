use ferro_core::Index;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tauri::State;

pub struct CoreState {
    inner: RwLock<Arc<Index>>,
}

impl CoreState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: RwLock::new(Arc::new(Index::new(root))),
        }
    }

    pub fn get(&self) -> Arc<Index> {
        self.inner.read().unwrap().clone()
    }

    pub fn set_root(&self, root: PathBuf) -> Arc<Index> {
        let idx = Arc::new(Index::new(root));
        *self.inner.write().unwrap() = idx.clone();
        idx
    }
}

#[tauri::command]
pub async fn get_stats(state: State<'_, CoreState>) -> Result<serde_json::Value, String> {
    let idx = state.get();
    let (n, ms) = idx.stats();
    Ok(serde_json::json!({"files": n, "indexed_ms": ms, "root": idx.root().to_string_lossy()}))
}

#[tauri::command]
pub async fn list_files(state: State<'_, CoreState>) -> Result<Vec<ferro_core::FileEntry>, String> {
    Ok(state.get().snapshot())
}

#[tauri::command]
pub async fn fuzzy(
    state: State<'_, CoreState>,
    q: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(50).min(200);
    let snap = state.get().snapshot();
    let paths: Vec<String> = snap.into_iter().map(|f| f.path).collect();
    let ranked = ferro_core::fuzzy::rank(&q, &paths, limit);
    Ok(serde_json::Value::Array(
        ranked
            .into_iter()
            .map(|(p, sc)| serde_json::json!({"path": p, "score": sc}))
            .collect(),
    ))
}

#[tauri::command]
pub async fn grep(
    state: State<'_, CoreState>,
    q: String,
    limit: Option<usize>,
) -> Result<Vec<ferro_core::search::Hit>, String> {
    let limit = limit.unwrap_or(50).min(200);
    let root = state.get().root().to_path_buf();
    tokio::task::spawn_blocking(move || ferro_core::search::grep(&root, &q, limit))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn read_file(state: State<'_, CoreState>, path: String) -> Result<String, String> {
    let idx = state.get();
    let Some(p) = idx.safe_join(&path) else {
        return Err("traversal blocked".into());
    };
    let t = tokio::fs::read_to_string(&p)
        .await
        .map_err(|_| "not found".to_string())?;
    if t.len() > 512 * 1024 {
        Ok(t[..512 * 1024].to_string())
    } else {
        Ok(t)
    }
}

#[tauri::command]
pub async fn git_status(state: State<'_, CoreState>) -> Result<String, String> {
    let root = state.get().root().to_path_buf();
    tokio::task::spawn_blocking(move || ferro_core::git::status(&root))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn git_diff(state: State<'_, CoreState>, path: Option<String>) -> Result<String, String> {
    let root = state.get().root().to_path_buf();
    tokio::task::spawn_blocking(move || ferro_core::git::diff_head(&root, path.as_deref()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_root(
    state: State<'_, CoreState>,
    path: String,
) -> Result<serde_json::Value, String> {
    let root = PathBuf::from(path);
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let idx = state.set_root(root);
    idx.rebuild().await;
    let (n, ms) = idx.stats();
    Ok(serde_json::json!({"files": n, "indexed_ms": ms, "root": idx.root().to_string_lossy()}))
}

#[tauri::command]
pub async fn reindex(state: State<'_, CoreState>) -> Result<serde_json::Value, String> {
    let idx = state.get();
    idx.rebuild().await;
    let (n, ms) = idx.stats();
    Ok(serde_json::json!({"files": n, "indexed_ms": ms, "root": idx.root().to_string_lossy()}))
}

#[tauri::command]
pub async fn pick_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let dir = app
        .dialog()
        .file()
        .set_title("Open folder in Ferro")
        .blocking_pick_folder();
    Ok(dir.map(|p| p.to_string()))
}

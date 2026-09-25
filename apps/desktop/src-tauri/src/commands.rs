use ferro_core::Index;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tauri::State;

pub struct CoreState {
    inner: RwLock<Arc<Index>>,
    /// Keeps a PR worktree's tempdir alive while reviewed.
    pr_hold: RwLock<Option<ferro_core::pr::PrWorktree>>,
}

impl CoreState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: RwLock::new(Arc::new(Index::new(root))),
            pr_hold: RwLock::new(None),
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
        Ok(ferro_core::text::truncate_utf8(&t, 512 * 1024).to_string())
    } else {
        Ok(t)
    }
}

#[tauri::command]
pub async fn file_meta(
    state: State<'_, CoreState>,
    path: String,
) -> Result<ferro_core::FileMeta, String> {
    state
        .get()
        .file_meta(&path)
        .ok_or_else(|| "not found".to_string())
}

#[tauri::command]
pub async fn read_window(
    state: State<'_, CoreState>,
    path: String,
    start: Option<usize>,
    count: Option<usize>,
) -> Result<ferro_core::Window, String> {
    let idx = state.get();
    let start = start.unwrap_or(0);
    let count = count.unwrap_or(200).clamp(1, 2000);
    tokio::task::spawn_blocking(move || idx.read_window(&path, start, count))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "not found".to_string())
}

#[tauri::command]
pub async fn highlight(
    state: State<'_, CoreState>,
    path: String,
    start: Option<usize>,
    count: Option<usize>,
) -> Result<ferro_core::highlight::HlWindow, String> {
    let idx = state.get();
    let start = start.unwrap_or(0);
    let count = count.unwrap_or(200).clamp(1, 1000);
    tokio::task::spawn_blocking(move || idx.highlight_window(&path, start, count))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "not found".to_string())
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
pub async fn ask(
    state: State<'_, CoreState>,
    question: String,
    max_steps: Option<usize>,
) -> Result<serde_json::Value, String> {
    if question.trim().is_empty() {
        return Err("empty question".into());
    }
    let provider =
        ferro_agent::OpenAiCompat::from_env(None, None, None).map_err(|e| e.to_string())?;
    let idx = state.get();
    let sandbox = ferro_agent::Sandbox::readonly(idx.root().to_path_buf());
    let agent = ferro_agent::Agent {
        index: idx.clone(),
        sandbox,
        client: std::sync::Arc::new(provider),
        max_steps: max_steps.unwrap_or(8).clamp(1, 16),
    };
    let t = agent.run(&question).await;
    let id = ferro_agent::new_id();
    let _ = ferro_agent::log_ask(idx.root(), &id, &question, &t, &[]);
    Ok(serde_json::json!({"id": id, "transcript": t}))
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

#[tauri::command]
pub async fn open_pr(
    state: State<'_, CoreState>,
    url: String,
) -> Result<serde_json::Value, String> {
    let info = ferro_core::pr::parse_pr_url(&url).ok_or("not a GitHub PR URL")?;
    let work = tokio::task::spawn_blocking(move || ferro_core::pr::worktree_for_pr(&info))
        .await
        .map_err(|e| e.to_string())??;
    let stats = {
        let idx = state.set_root(work.dir.clone());
        idx.rebuild().await;
        idx.set_pr(ferro_core::pr::PrCtx {
            owner: work.info.owner.clone(),
            repo: work.info.repo.clone(),
            number: work.info.number,
            base_ref: work.base_ref.clone(),
            base_sha: work.base_sha.clone(),
            head_sha: work.head_sha.clone(),
        });
        let (n, ms) = idx.stats();
        serde_json::json!({
            "files": n, "indexed_ms": ms, "root": idx.root().to_string_lossy(),
            "pr": idx.pr_ctx(),
        })
    };
    *state.pr_hold.write().unwrap() = Some(work);
    Ok(stats)
}

#[tauri::command]
pub async fn draft_add(
    path: String,
    line: usize,
    body: String,
) -> Result<ferro_agent::review::Draft, String> {
    if path.is_empty() || line == 0 || body.trim().is_empty() {
        return Err("path, line>0 and body required".into());
    }
    Ok(ferro_agent::drafts().add(path, line, body))
}

#[tauri::command]
pub async fn draft_list() -> Result<Vec<ferro_agent::review::Draft>, String> {
    Ok(ferro_agent::drafts().list())
}

#[tauri::command]
pub async fn draft_delete(id: String) -> Result<(), String> {
    if ferro_agent::drafts().remove(&id) {
        Ok(())
    } else {
        Err("no such draft".into())
    }
}

#[tauri::command]
pub async fn review_submit(
    state: State<'_, CoreState>,
    event: Option<String>,
    body: Option<String>,
) -> Result<String, String> {
    let idx = state.get();
    let pr = idx.pr_ctx().ok_or("not in PR mode")?;
    let drafts = ferro_agent::drafts().list();
    let out = tokio::task::spawn_blocking(move || {
        ferro_agent::submit_review(
            &pr.owner,
            &pr.repo,
            pr.number,
            &event.unwrap_or_else(|| "comment".into()),
            &body.unwrap_or_default(),
            &drafts,
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    ferro_agent::drafts().clear();
    Ok(out)
}

async fn git_run(
    state: State<'_, CoreState>,
    f: impl FnOnce(std::path::PathBuf) -> Result<String, String> + Send + 'static,
) -> Result<String, String> {
    let root = state.get().root().to_path_buf();
    tokio::task::spawn_blocking(move || f(root))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_stage(state: State<'_, CoreState>, paths: Vec<String>) -> Result<String, String> {
    git_run(state, move |r| ferro_core::git::stage(&r, &paths)).await
}

#[tauri::command]
pub async fn git_unstage(
    state: State<'_, CoreState>,
    paths: Vec<String>,
) -> Result<String, String> {
    git_run(state, move |r| ferro_core::git::unstage(&r, &paths)).await
}

#[tauri::command]
pub async fn git_commit(state: State<'_, CoreState>, message: String) -> Result<String, String> {
    git_run(state, move |r| ferro_core::git::commit(&r, &message)).await
}

#[tauri::command]
pub async fn git_push(state: State<'_, CoreState>) -> Result<String, String> {
    git_run(state, |r| ferro_core::git::push(&r)).await
}

#[tauri::command]
pub async fn git_pull(state: State<'_, CoreState>) -> Result<String, String> {
    git_run(state, |r| ferro_core::git::pull_ff(&r)).await
}
#[tauri::command]
pub async fn git_commit_message(state: State<'_, CoreState>) -> Result<String, String> {
    let provider =
        ferro_agent::OpenAiCompat::from_env(None, None, None).map_err(|e| e.to_string())?;
    let root = state.get().root().to_path_buf();
    let staged = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["diff", "--cached"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    })
    .await
    .map_err(|e| e.to_string())?;
    if staged.trim().is_empty() {
        return Err("nothing staged — stage files first".into());
    }
    let basis: String = staged.chars().take(6000).collect();
    provider
        .complete_simple(
            "Write a single conventional-commit message (type: subject, <=72 chars, imperative). Output only the message.",
            &format!("Diff:\n{basis}"),
        )
        .await
        .map(|m| m.lines().next().unwrap_or("").trim().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_settings(
    state: State<'_, CoreState>,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, String> {
    Ok(ferro_core::settings::Settings::new(state.get().root().to_path_buf()).get())
}

#[tauri::command]
pub async fn save_settings(
    state: State<'_, CoreState>,
    patch: std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, String> {
    ferro_core::settings::Settings::new(state.get().root().to_path_buf()).save(&patch)
}

#[tauri::command]
pub async fn markdown(state: State<'_, CoreState>, path: String) -> Result<String, String> {
    let idx = state.get();
    tokio::task::spawn_blocking(move || ferro_core::media::render_markdown(&idx, &path))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "not markdown".to_string())
}

#[tauri::command]
pub async fn read_image(state: State<'_, CoreState>, path: String) -> Result<String, String> {
    let idx = state.get();
    tokio::task::spawn_blocking(move || ferro_core::media::image_data_url(&idx, &path))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "not an image".to_string())
}

#[tauri::command]
pub async fn review_apply(state: State<'_, CoreState>) -> Result<serde_json::Value, String> {
    let idx = state.get();
    let pr = idx.pr_ctx().ok_or("not in PR mode")?;
    let provider =
        ferro_agent::OpenAiCompat::from_env(None, None, None).map_err(|e| e.to_string())?;
    let drafts = ferro_agent::drafts().list();
    if drafts.is_empty() {
        return Err("no drafts to apply".into());
    }
    let mut prompt = format!(
        "You are addressing {} code review comment(s) on PR #{} ({}/{}). For each comment, make the minimal edit with apply_patch (one call per fix, unified diff with `+++ b/<path>` lines). Do not commit. Reply with a short summary of what changed.\n\nComments:\n",
        drafts.len(), pr.number, pr.owner, pr.repo
    );
    for d in &drafts {
        prompt.push_str(&format!("- {}:{} — {}\n", d.path, d.line, d.body));
    }
    let mut sandbox = ferro_agent::Sandbox::readonly(idx.root().to_path_buf());
    sandbox.allow_write = true;
    let agent = ferro_agent::Agent {
        index: idx.clone(),
        sandbox,
        client: std::sync::Arc::new(provider),
        max_steps: 12,
    };
    let t = agent.run(&prompt).await;
    let applied = t
        .steps
        .iter()
        .flat_map(|st| st.calls.iter())
        .filter(|(c, r)| c.name == "apply_patch" && r.ok)
        .count();
    let id = ferro_agent::new_id();
    let _ = ferro_agent::log_ask(
        idx.root(),
        &id,
        &format!("batch apply {} drafts", drafts.len()),
        &t,
        &[],
    );
    idx.rebuild().await;
    Ok(serde_json::json!({"applied": applied, "transcript": t}))
}

#[tauri::command]
pub async fn pr_info(state: State<'_, CoreState>) -> Result<serde_json::Value, String> {
    Ok(match state.get().pr_ctx() {
        Some(pr) => serde_json::json!({"pr": pr}),
        None => serde_json::json!({"pr": null}),
    })
}

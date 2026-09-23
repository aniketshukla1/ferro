mod commands;

use commands::CoreState;
use std::path::PathBuf;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let state = CoreState::new(root.clone());
    // Warm index in background; UI loads instantly like the CLI server.
    let warm = state.get();
    tauri::async_runtime::spawn(async move {
        warm.rebuild().await;
    });

    tauri::Builder::default()
        .manage(state)
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_stats,
            commands::list_files,
            commands::fuzzy,
            commands::grep,
            commands::read_file,
            commands::file_meta,
            commands::read_window,
            commands::highlight,
            commands::git_status,
            commands::git_diff,
            commands::set_root,
            commands::reindex,
            commands::pick_folder,
            commands::open_pr,
            commands::draft_add,
            commands::draft_list,
            commands::draft_delete,
            commands::review_submit,
            commands::git_stage,
            commands::git_unstage,
            commands::git_commit,
            commands::git_commit_message,
            commands::git_push,
            commands::git_pull,
            commands::ask,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ferro");
}

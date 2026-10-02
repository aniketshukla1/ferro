//! Recently opened folders (API.md § 7.2): `recent-workspaces.json` in the state dir, newest
//! first. Serving a folder and `POST /workspace/open` record it; PR worktrees are not recorded.

use ferro_core::dirs::FerroDirs;
use std::path::Path;

const FILE: &str = "recent-workspaces.json";
const MAX: usize = 20;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Recent {
    pub root: String,
    #[serde(rename = "openedAt")]
    pub opened_at: u64,
}

/// Every recorded folder, newest first (a missing or unreadable file is an empty list).
pub fn load(dirs: &FerroDirs) -> Vec<Recent> {
    std::fs::read(dirs.state_dir.join(FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Move `root` to the front. Best effort: a failed write only loses the history.
pub fn record(dirs: &FerroDirs, root: &Path) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root = root.to_string_lossy().into_owned();
    let mut list = load(dirs);
    list.retain(|r| r.root != root);
    let opened_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    list.insert(0, Recent { root, opened_at });
    list.truncate(MAX);
    let Ok(body) = serde_json::to_vec_pretty(&list) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dirs.state_dir);
    // Write, then rename: another ferro process never reads a half-written file.
    let tmp = dirs
        .state_dir
        .join(format!("{FILE}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, dirs.state_dir.join(FILE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates_and_capped() {
        let home = tempfile::tempdir().unwrap();
        let dirs = FerroDirs::new(
            home.path().join("c"),
            home.path().join("s"),
            home.path().join("h"),
        );
        assert!(load(&dirs).is_empty());
        let roots: Vec<_> = (0..MAX + 3)
            .map(|i| {
                let p = home.path().join(format!("r{i}"));
                std::fs::create_dir_all(&p).unwrap();
                p
            })
            .collect();
        for r in &roots {
            record(&dirs, r);
        }
        record(&dirs, &roots[MAX]); // again: back to the front, not twice
        let list = load(&dirs);
        assert_eq!(list.len(), MAX);
        let canon = |p: &Path| p.canonicalize().unwrap().to_string_lossy().into_owned();
        assert_eq!(list[0].root, canon(&roots[MAX]));
        assert_eq!(list[1].root, canon(&roots[MAX + 2]));
        assert_eq!(list.iter().filter(|r| r.root == list[0].root).count(), 1);
        assert!(
            !list.iter().any(|r| r.root == canon(&roots[0])),
            "the oldest fell off"
        );
    }
}

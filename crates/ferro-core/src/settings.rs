//! Workspace settings in the ferro state dir:
//! `state_dir/workspaces/<key>/settings.json`.
//! A legacy `{root}/.ferro/config.json` is read once if present, never written.
//! Visual editor + raw JSON both read/write; unknown keys preserved.

use serde_json::Value;
use std::collections::BTreeMap;

use crate::dirs::FerroDirs;

#[derive(Debug, Clone)]
pub struct Settings {
    root: std::path::PathBuf,
    dirs: FerroDirs,
    key: String,
}

impl Settings {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self::with_dirs(root, FerroDirs::resolve())
    }

    pub fn with_dirs(root: std::path::PathBuf, dirs: FerroDirs) -> Self {
        let root = root.canonicalize().unwrap_or(root);
        let key = dirs.workspace_key(&root);
        Self { root, dirs, key }
    }

    fn path(&self) -> std::path::PathBuf {
        self.dirs
            .workspace_state_dir(&self.key)
            .join("settings.json")
    }

    fn legacy_path(&self) -> std::path::PathBuf {
        self.root.join(".ferro").join("config.json")
    }

    pub fn defaults() -> BTreeMap<String, Value> {
        [
            ("theme".to_string(), Value::String("forge".into())),
            ("wordWrap".to_string(), Value::Bool(false)),
            ("sidebar".to_string(), Value::Bool(true)),
            ("askMaxSteps".to_string(), Value::from(8)),
            ("fuzzyLimit".to_string(), Value::from(50)),
        ]
        .into_iter()
        .collect()
    }

    fn read_file(path: &std::path::Path) -> BTreeMap<String, Value> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    /// Effective settings = defaults < legacy file < state file.
    pub fn get(&self) -> BTreeMap<String, Value> {
        let mut out = Self::defaults();
        for (k, v) in Self::read_file(&self.legacy_path()) {
            out.insert(k, v);
        }
        for (k, v) in Self::read_file(&self.path()) {
            out.insert(k, v);
        }
        out
    }

    /// Merge a partial object over the state file. Returns effective map.
    /// The legacy file is never written.
    pub fn save(&self, patch: &BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, String> {
        let mut cur = Self::read_file(&self.path());
        for (k, v) in patch {
            cur.insert(k.clone(), v.clone());
        }
        // Validate known keys lightly; keep unknowns.
        if let Some(Value::String(t)) = cur.get("theme") {
            const KNOWN: &[&str] = &[
                "forge", "paper", "mocha", "nord", "dracula", "gruvbox", "tokyo",
            ];
            if !KNOWN.contains(&t.as_str()) {
                return Err(format!("unknown theme: {t}"));
            }
        }
        if let Some(dir) = self.path().parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let obj: serde_json::Map<String, Value> = cur.into_iter().collect();
        atomic_write(
            &self.path(),
            serde_json::to_string_pretty(&obj)
                .unwrap_or_default()
                .as_bytes(),
        )?;
        Ok(self.get())
    }
}

/// Atomic write (temp file + rename) for all JSON state files.
pub fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> (tempfile::TempDir, tempfile::TempDir, Settings) {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let dirs = FerroDirs::new(
            home.path().join("c"),
            home.path().join("s"),
            home.path().join("h"),
        );
        let s = Settings::with_dirs(root.path().to_path_buf(), dirs);
        (root, home, s)
    }

    #[test]
    fn defaults_merge_and_validate() {
        let (_root, _home, s) = settings();
        assert_eq!(s.get()["theme"], Value::String("forge".into()));
        let eff = s
            .save(
                &[("theme".into(), Value::String("dracula".into()))]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
        assert_eq!(eff["theme"], Value::String("dracula".into()));
        assert!(eff.contains_key("wordWrap"));
        assert!(s
            .save(
                &[("theme".into(), Value::String("nope".into()))]
                    .into_iter()
                    .collect()
            )
            .is_err());
    }

    #[test]
    fn legacy_read_once_never_written() {
        let (root, _home, s) = settings();
        std::fs::create_dir_all(root.path().join(".ferro")).unwrap();
        std::fs::write(
            root.path().join(".ferro/config.json"),
            r#"{"theme":"paper"}"#,
        )
        .unwrap();
        assert_eq!(s.get()["theme"], Value::String("paper".into()));
        s.save(
            &[("sidebar".into(), Value::Bool(false))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        // Legacy file untouched; state file holds the new key.
        let legacy = std::fs::read_to_string(root.path().join(".ferro/config.json")).unwrap();
        assert!(!legacy.contains("sidebar"));
        assert_eq!(s.get()["sidebar"], Value::Bool(false));
        assert_eq!(s.get()["theme"], Value::String("paper".into()));
    }
}

//! Workspace settings: `{root}/.ferro/config.json`.
//! Visual editor + raw JSON both read/write this file. Unknown keys preserved.

use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Settings {
    pub root: std::path::PathBuf,
}

impl Settings {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self { root }
    }

    fn path(&self) -> std::path::PathBuf {
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

    /// Effective settings = defaults merged under stored file values.
    pub fn get(&self) -> BTreeMap<String, Value> {
        let mut out = Self::defaults();
        if let Ok(text) = std::fs::read_to_string(self.path()) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                for (k, v) in map {
                    out.insert(k, v);
                }
            }
        }
        out
    }

    /// Merge a partial object over the stored file. Returns effective map.
    pub fn save(&self, patch: &BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, String> {
        let mut cur: BTreeMap<String, Value> = (|| {
            let text = std::fs::read_to_string(self.path()).ok()?;
            match serde_json::from_str::<Value>(&text).ok()? {
                Value::Object(m) => Some(m.into_iter().collect()),
                _ => None,
            }
        })()
        .unwrap_or_default();
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
        std::fs::write(
            self.path(),
            serde_json::to_string_pretty(&obj).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        Ok(self.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_merge_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::new(dir.path().to_path_buf());
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
}

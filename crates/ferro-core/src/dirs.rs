//! Ferro directories per BACKEND.md § 4.5. Nothing is ever written
//! inside the user's repository; all state lives here.
//!
//! ```text
//! config_dir = $FERRO_HOME            | $XDG_CONFIG_HOME/ferro | ~/.ferro
//! state_dir  = $FERRO_HOME/state      | $XDG_STATE_HOME/ferro  | ~/.ferro/state
//! cache_dir  = $FERRO_HOME/cache      | $XDG_CACHE_HOME/ferro  | ~/.ferro/cache
//! ```

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FerroDirs {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl FerroDirs {
    pub fn new(config_dir: PathBuf, state_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            config_dir,
            state_dir,
            cache_dir,
        }
    }

    /// Resolve from the environment. Last resort is the temp dir, never the repo.
    pub fn resolve() -> Self {
        if let Ok(home) = std::env::var("FERRO_HOME") {
            if !home.is_empty() {
                let home = PathBuf::from(home);
                return Self::new(home.clone(), home.join("state"), home.join("cache"));
            }
        }
        let base = |xdg: &str, dot: &str| {
            std::env::var(xdg)
                .map(PathBuf::from)
                .unwrap_or_else(|_| home_dir().join(dot))
                .join("ferro")
        };
        Self::new(
            base("XDG_CONFIG_HOME", ".config"),
            {
                let s = std::env::var("XDG_STATE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home_dir().join(".local").join("state"));
                s.join("ferro")
            },
            {
                let c = std::env::var("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home_dir().join(".cache"));
                c.join("ferro")
            },
        )
    }

    pub fn workspace_key(&self, root: &Path) -> String {
        use sha2::Digest as _;
        let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut h = sha2::Sha256::new();
        h.update(canon.to_string_lossy().as_bytes());
        let hex = format!("{:x}", h.finalize());
        hex[..16].to_string()
    }

    pub fn workspace_state_dir(&self, key: &str) -> PathBuf {
        self.state_dir.join("workspaces").join(key)
    }

    pub fn workspace_cache_dir(&self, key: &str) -> PathBuf {
        self.cache_dir.join("workspaces").join(key)
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("ferro-home"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_and_key_stability() {
        let t = tempfile::tempdir().unwrap();
        let d = FerroDirs::new(t.path().join("c"), t.path().join("s"), t.path().join("h"));
        let a = tempfile::tempdir().unwrap();
        let k1 = d.workspace_key(a.path());
        let k2 = d.workspace_key(a.path());
        assert_eq!(k1.len(), 16);
        assert_eq!(k1, k2);
        assert!(k1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn workspace_layout() {
        let t = tempfile::tempdir().unwrap();
        let d = FerroDirs::new(t.path().join("c"), t.path().join("s"), t.path().join("h"));
        assert_eq!(
            d.workspace_state_dir("abc"),
            t.path().join("s/workspaces/abc")
        );
        assert_eq!(
            d.workspace_cache_dir("abc"),
            t.path().join("h/workspaces/abc")
        );
    }
}

//! Running ferro servers, one record per served folder, so `ferro open` (and the editor
//! extensions) reuse the server already showing a folder instead of starting another one.
//! Records hold the session token, so they are written readable by the user only.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Instance {
    pub pid: u32,
    /// Canonical folder being served.
    pub root: String,
    /// `http://127.0.0.1:7778` plus any base path, without a trailing slash.
    pub base: String,
    pub token: String,
}

fn file(state_dir: &Path, key: &str) -> PathBuf {
    state_dir.join("instances").join(format!("{key}.json"))
}

/// Record a server that has started (atomically, mode 0600).
pub fn record(state_dir: &Path, key: &str, inst: &Instance) -> std::io::Result<PathBuf> {
    let p = file(state_dir, key);
    let dir = p.parent().unwrap_or(state_dir);
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{key}.{}.tmp", inst.pid));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        std::io::Write::write_all(&mut f, serde_json::to_string(inst)?.as_bytes())?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Drop the record when this process wrote it (a newer server may have replaced it).
pub fn forget(state_dir: &Path, key: &str, pid: u32) {
    let p = file(state_dir, key);
    let ours = std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice::<Instance>(&b).ok())
        .is_some_and(|i| i.pid == pid);
    if ours {
        let _ = std::fs::remove_file(p);
    }
}

/// The live server for `key`: its record must answer `/api/v1/meta` with its token and serve
/// the same folder (so a port reused by another server never gets this folder's links).
/// Stale records are removed.
pub async fn find(state_dir: &Path, key: &str) -> Option<Instance> {
    let p = file(state_dir, key);
    let inst: Instance = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
    let live = async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(1500))
            .build()
            .ok()?;
        let meta: serde_json::Value = client
            .get(format!("{}/api/v1/meta", inst.base))
            .bearer_auth(&inst.token)
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success())?
            .json()
            .await
            .ok()?;
        (meta["workspace"]["root"].as_str() == Some(inst.root.as_str())).then_some(())
    };
    if live.await.is_some() {
        Some(inst)
    } else {
        let _ = std::fs::remove_file(&p);
        None
    }
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The link that opens `path` (at `line`) and optionally a view in a running server.
pub fn open_url(
    inst: &Instance,
    path: Option<&str>,
    line: Option<usize>,
    view: Option<&str>,
) -> String {
    let mut u = format!("{}/?token={}", inst.base, encode(&inst.token));
    if let Some(p) = path {
        u.push_str(&format!("&path={}", encode(p)));
        if let Some(l) = line {
            u.push_str(&format!("&line={l}"));
        }
    }
    if let Some(v) = view {
        u.push_str(&format!("&view={}", encode(v)));
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(base: &str) -> Instance {
        Instance {
            pid: std::process::id(),
            root: "/work/app".into(),
            base: base.into(),
            token: "t0k".into(),
        }
    }

    #[test]
    fn links_carry_the_file_line_and_view() {
        let i = inst("http://127.0.0.1:7778");
        assert_eq!(
            open_url(&i, Some("src/a b.rs"), Some(12), Some("diff")),
            "http://127.0.0.1:7778/?token=t0k&path=src/a%20b.rs&line=12&view=diff"
        );
        assert_eq!(
            open_url(&i, None, Some(3), None),
            "http://127.0.0.1:7778/?token=t0k"
        );
    }

    #[tokio::test]
    async fn records_are_private_and_stale_ones_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing listens on port 9 (discard): the record is stale.
        let p = record(dir.path(), "k1", &inst("http://127.0.0.1:9")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(find(dir.path(), "k1").await.is_none());
        assert!(!p.exists());
        // forget() leaves another process's record alone.
        let mut other = inst("http://127.0.0.1:9");
        other.pid += 1;
        let p = record(dir.path(), "k2", &other).unwrap();
        forget(dir.path(), "k2", std::process::id());
        assert!(p.exists());
    }
}

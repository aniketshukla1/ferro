//! GitHub PR review support: parse PR URLs, materialize an ephemeral
//! worktree (head + base fetched), and diff against the merge-base.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PrInfo {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrCtx {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub base_ref: String,
    pub base_sha: String,
    pub head_sha: String,
}

#[derive(Debug)]
pub struct PrWorktree {
    /// Keeps the tempdir alive for the serve lifetime.
    #[allow(dead_code)]
    pub _tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub info: PrInfo,
    pub base_ref: String,
    pub base_sha: String,
    pub head_sha: String,
}

pub fn parse_pr_url(s: &str) -> Option<PrInfo> {
    let s = s.trim().trim_end_matches('/').to_lowercase();
    let rest = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))?;
    let mut parts = rest.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next()? != "pull" {
        return None;
    }
    let number: u64 = parts.next()?.parse().ok()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(PrInfo {
        owner,
        repo,
        number,
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Base ref via `gh` (authenticated, exact), else `main`.
fn base_ref(info: &PrInfo, repo_dir: &Path) -> String {
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            &info.number.to_string(),
            "--repo",
            &format!("{}/{}", info.owner, info.repo),
            "--json",
            "baseRefName",
            "--jq",
            ".baseRefName",
        ])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    // Fallback: probe remote default branch.
    git(
        repo_dir,
        &[
            "ls-remote",
            "--symref",
            &format!("https://github.com/{}/{}.git", info.owner, info.repo),
            "HEAD",
        ],
    )
    .ok()
    .and_then(|s| {
        s.lines().find_map(|l| {
            l.strip_prefix("ref: refs/heads/")
                .and_then(|r| r.split_whitespace().next().map(|b| b.to_string()))
        })
    })
    .unwrap_or_else(|| "main".into())
}

pub fn worktree_for_pr(info: &PrInfo) -> Result<PrWorktree, String> {
    let tmp = tempfile::TempDir::new().map_err(|e| e.to_string())?;
    let dir = tmp.path().to_path_buf();
    let remote = format!("https://github.com/{}/{}.git", info.owner, info.repo);
    // Neutral init branch: fetching base into its own name must never collide.
    git(&dir, &["init", "-q", "-b", "ferro-root"])?;
    git(&dir, &["remote", "add", "origin", &remote])?;
    // PR head.
    git(
        &dir,
        &[
            "fetch",
            "--depth",
            "100",
            "origin",
            &format!("pull/{}/head:pr-head", info.number),
        ],
    )
    .map_err(|e| format!("fetch PR head failed (private repo? set gh auth): {e}"))?;
    // Base branch for merge-base, fetched under its own name so
    // `git merge-base HEAD <base>` resolves inside the worktree.
    let base = base_ref(info, &dir);
    git(
        &dir,
        &["fetch", "--depth", "100", "origin", &format!("{base}:{base}")],
    )
    .map_err(|e| format!("fetch base {base} failed: {e}"))?;
    git(&dir, &["checkout", "-q", "pr-head"])?;
    let base_sha = git(&dir, &["rev-parse", &base])?;
    let head_sha = git(&dir, &["rev-parse", "HEAD"])?;
    Ok(PrWorktree {
        _tmp: tmp,
        dir,
        info: info.clone(),
        base_ref: base,
        base_sha,
        head_sha,
    })
}

/// `git diff <merge-base>..HEAD` — the scoped PR diff px0 shows.
pub fn diff_merge_base(dir: &Path, base_ref: &str, path: Option<&str>) -> String {
    let base = git(dir, &["merge-base", "HEAD", base_ref]).unwrap_or_else(|_| base_ref.to_string());
    let mut owned = vec![
        "diff".to_string(),
        base,
        "HEAD".to_string(),
        "--".to_string(),
    ];
    if let Some(p) = path {
        owned.push(p.to_string());
    }
    let out = Command::new("git").arg("-C").arg(dir).args(&owned).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(e) => format!("git error: {e}"),
    }
}

pub fn changed_files(dir: &Path, base_ref: &str) -> Vec<String> {
    let base = git(dir, &["merge-base", "HEAD", base_ref]).unwrap_or_else(|_| base_ref.to_string());
    git(dir, &["diff", "--name-only", &base, "HEAD"])
        .unwrap_or_default()
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_urls() {
        let i = parse_pr_url("https://github.com/owner/repo/pull/123").unwrap();
        assert_eq!(i.owner, "owner");
        assert_eq!(i.repo, "repo");
        assert_eq!(i.number, 123);
        assert!(parse_pr_url("https://github.com/owner/repo/issues/1").is_none());
        assert!(parse_pr_url("/some/path").is_none());
    }

    #[test]
    fn touched_diff_helpers_need_repo() {
        // merge-base helpers shell out; url parsing is the unit under test here.
        assert!(parse_pr_url("px0 https://github.com/a/b/pull/9").is_none());
    }
}

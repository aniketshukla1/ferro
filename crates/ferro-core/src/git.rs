use std::path::Path;
use std::process::Command;

pub fn status(root: &Path) -> String {
    run(root, &["status", "--porcelain=v1", "-b"])
}

pub fn diff_head(root: &Path, rel: Option<&str>) -> String {
    match rel {
        Some(r) => run(root, &["diff", "HEAD", "--", r]),
        None => run(root, &["diff", "HEAD"]),
    }
}

#[allow(dead_code)]
pub fn merge_base(root: &Path, target: &str) -> String {
    run(root, &["merge-base", "HEAD", target])
}

fn run(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(root).args(args).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(e) => format!("git error: {e}"),
    }
}

fn run_check(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git error: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

/// Dry-run a unified diff against the working tree. Ok(()) means it applies cleanly.
pub fn apply_check(root: &Path, patch: &str) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    use std::io::Write;
    f.write_all(patch.as_bytes()).map_err(|e| e.to_string())?;
    run_check(
        root,
        &["apply", "--check", f.path().to_string_lossy().as_ref()],
    )
    .map(|_| ())
    .map_err(|e| format!("patch does not apply: {e}"))
}

/// Apply a unified diff to the working tree (not committed).
pub fn apply(root: &Path, patch: &str) -> Result<(), String> {
    apply_check(root, patch)?;
    let mut f = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    use std::io::Write;
    f.write_all(patch.as_bytes()).map_err(|e| e.to_string())?;
    run_check(root, &["apply", f.path().to_string_lossy().as_ref()]).map(|_| ())
}

/// Revert paths to HEAD and delete listed untracked files.
pub fn revert(root: &Path, tracked: &[String], untracked: &[String]) -> Result<(), String> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut safe_tracked = Vec::with_capacity(tracked.len());
    for t in tracked {
        let p = crate::paths::resolve(&root_canon, t, crate::paths::Access::Write)
            .map_err(|e| e.to_string())?;
        let rel = p
            .strip_prefix(&root_canon)
            .map_err(|_| "path escapes root".to_string())?;
        safe_tracked.push(rel.to_string_lossy().to_string());
    }
    if !safe_tracked.is_empty() {
        let mut args = vec!["checkout", "HEAD", "--"];
        args.extend(safe_tracked.iter().map(|s| s.as_str()));
        run_check(root, &args)?;
    }
    for u in untracked {
        let p = crate::paths::resolve(&root_canon, u, crate::paths::Access::Write)
            .map_err(|e| e.to_string())?;
        if p.is_file() {
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn resolve_write_args(root: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    paths
        .iter()
        .map(|p| {
            let abs = crate::paths::resolve(&root_canon, p, crate::paths::Access::Write)
                .map_err(|e| e.to_string())?;
            abs.strip_prefix(&root_canon)
                .map(|r| r.to_string_lossy().to_string())
                .map_err(|_| "path escapes root".to_string())
        })
        .collect()
}

pub fn stage(root: &Path, paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("no paths".into());
    }
    let safe = resolve_write_args(root, paths)?;
    let mut args = vec!["add", "--"];
    args.extend(safe.iter().map(|s| s.as_str()));
    run_check(root, &args)
}

pub fn unstage(root: &Path, paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("no paths".into());
    }
    let safe = resolve_write_args(root, paths)?;
    let mut args = vec!["reset", "HEAD", "--"];
    args.extend(safe.iter().map(|s| s.as_str()));
    run_check(root, &args)
}

pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("empty message".into());
    }
    let staged = run_check(root, &["diff", "--cached", "--quiet"]).is_err();
    if !staged {
        return Err("nothing staged".into());
    }
    run_check(root, &["commit", "-m", message])
}

pub fn push(root: &Path) -> Result<String, String> {
    run_check(root, &["push"])
}

pub fn pull_ff(root: &Path) -> Result<String, String> {
    run_check(root, &["pull", "--ff-only"])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(r)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(r.join("a.txt"), "one\n").unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["commit", "-m", "init"])
            .status()
            .unwrap()
            .success());
        dir
    }

    #[test]
    fn stage_unstage_commit() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        stage(dir.path(), &["a.txt".to_string()]).unwrap();
        assert!(status(dir.path()).contains('M'));
        unstage(dir.path(), &["a.txt".to_string()]).unwrap();
        commit(dir.path(), "should fail").unwrap_err();
        stage(dir.path(), &["a.txt".to_string()]).unwrap();
        let out = commit(dir.path(), "second").unwrap();
        assert!(out.contains("second") || out.contains("1 file changed"));
    }
}

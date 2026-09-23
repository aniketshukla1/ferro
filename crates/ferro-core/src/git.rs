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
    if !tracked.is_empty() {
        let mut args = vec!["checkout", "HEAD", "--"];
        args.extend(tracked.iter().map(|s| s.as_str()));
        run_check(root, &args)?;
    }
    for u in untracked {
        let p = root.join(u.trim_start_matches('/'));
        if p.is_file() {
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

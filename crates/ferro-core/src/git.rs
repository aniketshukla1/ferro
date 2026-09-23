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

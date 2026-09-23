//! Patch apply with snapshot + rollback.
//! Flow: parse touched paths -> sandbox-check each -> snapshot existence ->
//! `git apply --check` -> `git apply` -> report. `revert()` restores tracked
//! files to HEAD and deletes files the patch created.

use serde::Serialize;

use crate::policy::{Access, Sandbox};

#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub files: Vec<String>,
    pub created: Vec<String>,
    pub status_after: String,
}

/// Normalize a unified diff for `git apply`:
/// inject `new file mode 100644` for creations (`--- /dev/null`) that omit it.
/// LLM-generated patches often drop the extended headers git wants.
pub fn normalize(patch: &str) -> String {
    let mut out = String::with_capacity(patch.len() + 128);
    let mut in_diff = false;
    let mut has_mode_line = false;
    for line in patch.lines() {
        if let Some(_rest) = line.strip_prefix("diff --git ") {
            in_diff = true;
            has_mode_line = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_diff
            && (line.starts_with("new file mode")
                || line.starts_with("deleted file mode")
                || line.starts_with("old mode")
                || line.starts_with("new mode")
                || line.starts_with("index "))
        {
            has_mode_line = true;
        }
        if in_diff && line.starts_with("--- /dev/null") && !has_mode_line {
            out.push_str("new file mode 100644\n");
            has_mode_line = true;
        }
        out.push_str(line);
        out.push('\n');
        if line.starts_with("@@") {
            in_diff = false;
        }
    }
    out
}

/// Paths a unified diff touches, parsed from `+++ b/<path>` lines.
/// `/dev/null` targets (deletions) are skipped — deletions need --allow-destructive.
pub fn touched_files(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix("+++ ") else {
            continue;
        };
        let path = rest.split_whitespace().next().unwrap_or("");
        let path = path.strip_prefix("b/").unwrap_or(path);
        if path == "/dev/null" || path.is_empty() {
            continue;
        }
        if !out.iter().any(|p: &String| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

pub fn apply(
    root: &std::path::Path,
    sandbox: &Sandbox,
    patch: &str,
) -> Result<ApplyReport, String> {
    if patch.trim().is_empty() {
        return Err("empty patch".into());
    }
    let files = touched_files(patch);
    if files.is_empty() {
        return Err("no files found in patch (need `+++ b/<path>` lines)".into());
    }
    for f in &files {
        sandbox
            .resolve(f, Access::Write)
            .map_err(|e| format!("{f}: {e}"))?;
    }
    // Snapshot: which touched files exist before apply (the rest are created).
    let created: Vec<String> = files
        .iter()
        .filter(|f| !root.join(f.trim_start_matches('/')).exists())
        .cloned()
        .collect();
    ferro_core::git::apply(root, &normalize(patch))?;
    Ok(ApplyReport {
        files,
        created,
        status_after: ferro_core::git::status(root),
    })
}

pub fn revert(
    root: &std::path::Path,
    sandbox: &Sandbox,
    report: &ApplyReport,
) -> Result<(), String> {
    sandbox.check(Access::Write).map_err(|e| e.to_string())?;
    let tracked: Vec<String> = report
        .files
        .iter()
        .filter(|f| !report.created.iter().any(|c| c == *f))
        .cloned()
        .collect();
    ferro_core::git::revert(root, &tracked, &report.created)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(r)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(r.join("a.txt"), "one\n").unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["commit", "-m", "init"])
            .status()
            .unwrap()
            .success());
        dir
    }

    const PATCH: &str = concat!(
        "diff --git a/a.txt b/a.txt\n",
        "--- a/a.txt\n",
        "+++ b/a.txt\n",
        "@@ -1 +1,2 @@\n",
        " one\n",
        "+two\n",
        "diff --git a/new.txt b/new.txt\n",
        "--- /dev/null\n",
        "+++ b/new.txt\n",
        "@@ -0,0 +1 @@\n",
        "+hello\n",
    );

    #[test]
    fn parses_touched() {
        let f = touched_files(PATCH);
        assert_eq!(f, vec!["a.txt", "new.txt"]);
    }

    #[test]
    fn apply_and_revert() {
        let dir = git_repo();
        let sb = Sandbox::readonly(dir.path().to_path_buf());
        let mut sb = sb;
        sb.allow_write = true;
        let rep = apply(dir.path(), &sb, PATCH).unwrap();
        assert_eq!(rep.files, vec!["a.txt", "new.txt"]);
        assert_eq!(rep.created, vec!["new.txt"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert!(dir.path().join("new.txt").exists());
        revert(dir.path(), &sb, &rep).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(!dir.path().join("new.txt").exists());
    }

    #[test]
    fn bad_patch_rejected_before_touching_tree() {
        let dir = git_repo();
        let sb = Sandbox::readonly(dir.path().to_path_buf());
        let mut sb = sb;
        sb.allow_write = true;
        let err = apply(dir.path(), &sb, "not a patch").unwrap_err();
        assert!(err.contains("no files"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );
    }

    #[test]
    fn write_disabled_blocks_apply() {
        let dir = git_repo();
        let sb = Sandbox::readonly(dir.path().to_path_buf());
        let err = apply(dir.path(), &sb, PATCH).unwrap_err();
        assert!(err.contains("a.txt"));
    }
}

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

/// One file section of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// New-side path (`b/…`); for deletions, the removed path.
    pub path: String,
    /// Old-side path for renames.
    pub old_path: Option<String>,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modify,
    Create,
    Delete,
    Rename,
}

/// Parse every `diff --git` section. Deletions (`+++ /dev/null`) and renames
/// are reported — they require `allow_destructive` (D21).
pub fn changes(patch: &str) -> Vec<FileChange> {
    let mut out: Vec<FileChange> = Vec::new();
    let mut cur: Option<FileChange> = None;
    let mut rename_from: Option<String> = None;
    let flush = |cur: &mut Option<FileChange>, out: &mut Vec<FileChange>| {
        if let Some(c) = cur.take() {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    };
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            flush(&mut cur, &mut out);
            rename_from = None;
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            rename_from = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            let to = rest.trim().to_string();
            cur = Some(FileChange {
                path: to,
                old_path: rename_from.take(),
                kind: ChangeKind::Rename,
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            let target = rest.split_whitespace().next().unwrap_or("");
            if target == "/dev/null" {
                // Deletion: path comes from the `--- a/…` line.
                if let Some(c) = cur.as_mut() {
                    c.kind = ChangeKind::Delete;
                } else {
                    cur = Some(FileChange {
                        path: String::new(),
                        old_path: None,
                        kind: ChangeKind::Delete,
                    });
                }
            } else {
                let path = target.strip_prefix("b/").unwrap_or(target).to_string();
                match cur.as_mut() {
                    Some(c) if c.path.is_empty() => {
                        c.path = path;
                    }
                    // Rename sections also carry ---/+++ lines; keep the Rename.
                    Some(c) if c.kind == ChangeKind::Rename => {}
                    _ => {
                        flush(&mut cur, &mut out);
                        cur = Some(FileChange {
                            path,
                            old_path: None,
                            kind: ChangeKind::Modify,
                        });
                    }
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            let target = rest.split_whitespace().next().unwrap_or("");
            if target == "/dev/null" {
                // Creation: path comes from the `+++ b/…` line.
                if cur.is_none() {
                    cur = Some(FileChange {
                        path: String::new(),
                        old_path: None,
                        kind: ChangeKind::Create,
                    });
                } else if let Some(c) = cur.as_mut() {
                    if c.kind == ChangeKind::Modify {
                        c.kind = ChangeKind::Create;
                    }
                }
            } else {
                let path = target.strip_prefix("a/").unwrap_or(target).to_string();
                // Rename sections carry their own paths; don't overwrite them.
                if cur.is_none() {
                    cur = Some(FileChange {
                        path: path.clone(),
                        old_path: None,
                        kind: ChangeKind::Modify,
                    });
                }
            }
            continue;
        }
    }
    flush(&mut cur, &mut out);
    out.retain(|c| !c.path.is_empty());
    out
}

/// Paths a unified diff touches (all kinds, deduplicated).
pub fn touched_files(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in changes(patch) {
        for p in [c.old_path, Some(c.path)].into_iter().flatten() {
            if !p.is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// True when the patch deletes or renames files (needs `allow_destructive`).
pub fn needs_destructive(patch: &str) -> bool {
    changes(patch)
        .iter()
        .any(|c| matches!(c.kind, ChangeKind::Delete | ChangeKind::Rename))
}

pub fn apply(
    root: &std::path::Path,
    sandbox: &Sandbox,
    patch: &str,
) -> Result<ApplyReport, String> {
    if patch.trim().is_empty() {
        return Err("empty patch".into());
    }
    let file_changes = changes(patch);
    if file_changes.is_empty() {
        return Err("no files found in patch (need `diff --git` sections)".into());
    }
    if needs_destructive(patch) && !sandbox.allow_destructive {
        return Err("patch deletes or renames files (restart with --allow-destructive)".into());
    }
    let files = touched_files(patch);
    for f in &files {
        // Deletions resolve against the old path, which must exist.
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

    const DEL_PATCH: &str = concat!(
        "diff --git a/gone.txt b/gone.txt\n",
        "deleted file mode 100644\n",
        "index 257cc56..0000000\n",
        "--- a/gone.txt\n",
        "+++ /dev/null\n",
        "@@ -1 +0,0 @@\n",
        "-bye\n",
    );

    const RENAME_PATCH: &str = concat!(
        "diff --git a/old.txt b/new2.txt\n",
        "similarity index 90%\n",
        "rename from old.txt\n",
        "rename to new2.txt\n",
        "--- a/old.txt\n",
        "+++ b/new2.txt\n",
        "@@ -1 +1 @@\n",
        "-hi\n",
        "+yo\n",
    );

    #[test]
    fn parses_delete_and_rename() {
        let d = changes(DEL_PATCH);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, ChangeKind::Delete);
        assert_eq!(d[0].path, "gone.txt");
        assert!(needs_destructive(DEL_PATCH));
        let r = changes(RENAME_PATCH);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, ChangeKind::Rename);
        assert_eq!(r[0].old_path.as_deref(), Some("old.txt"));
        assert!(needs_destructive(RENAME_PATCH));
        assert!(!needs_destructive(PATCH));
    }

    #[test]
    fn delete_needs_destructive_flag() {
        let dir = git_repo();
        std::fs::write(dir.path().join("gone.txt"), "bye\n").unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "-m", "add"])
            .status()
            .unwrap()
            .success());
        let sb = Sandbox::readonly(dir.path().to_path_buf());
        let mut sb = sb;
        sb.allow_write = true;
        assert!(apply(dir.path(), &sb, DEL_PATCH).is_err());
        sb.allow_destructive = true;
        apply(dir.path(), &sb, DEL_PATCH).unwrap();
        assert!(!dir.path().join("gone.txt").exists());
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

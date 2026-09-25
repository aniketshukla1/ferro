//! Path resolution per BACKEND.md § 5.1.
//! Every client-supplied path goes through [`resolve`]. Symlinks never escape.

use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    #[error("empty path")]
    Empty,
    #[error("path escapes the workspace root")]
    Escapes,
    #[error("protected path")]
    Protected,
}

pub fn resolve(root: &Path, rel: &str, access: Access) -> Result<PathBuf, PathError> {
    // Canonical root when it exists so every later comparison is consistent
    // (macOS /tmp is a symlink). When the root itself is missing there is
    // nothing to symlink through below it, so the lexical check below stands.
    let root_canon = root.canonicalize().ok();
    let root_ref: &Path = root_canon.as_deref().unwrap_or(root);
    let rel = rel.trim().replace('\\', "/");
    if rel.starts_with('/') {
        return Err(PathError::Escapes);
    }
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        return Err(PathError::Empty);
    }
    if rel.as_bytes().contains(&0) {
        return Err(PathError::Escapes);
    }
    // Reject `..`, absolute forms, and Windows drive/UNC prefixes lexically first.
    for comp in rel.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            return Err(PathError::Escapes);
        }
    }
    if rel.len() >= 2 {
        let b = rel.as_bytes();
        // `C:/…`, `C:…`, `\\server\share`
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return Err(PathError::Escapes);
        }
        if rel.starts_with("//") || rel.starts_with("\\\\") {
            return Err(PathError::Escapes);
        }
    }
    if access == Access::Write {
        for seg in rel.split('/').filter(|s| !s.is_empty() && *s != ".") {
            if seg == ".git" || seg == ".hg" || seg == ".svn" {
                return Err(PathError::Protected);
            }
        }
    }
    // Join to the root. When the root is canonical, every comparison below
    // is symlink-aware; otherwise the lexical containment check stands alone.
    let joined = root_ref.join(rel);
    // If the target exists: canonicalize and require the prefix (symlink check).
    if let Ok(canon) = joined.canonicalize() {
        if root_canon.is_some() {
            let rc = root_canon.as_deref().unwrap();
            if canon == rc || canon.starts_with(rc) {
                return Ok(canon);
            }
            return Err(PathError::Escapes);
        }
        return Ok(canon);
    }
    // Missing target (writes, new files): canonicalize the nearest existing
    // ancestor. When the root itself is missing there is nothing below it to
    // symlink through, so the lexical containment above stands on its own.
    let root_missing = root_canon.is_none();
    let mut ancestor = joined.as_path();
    let mut tail: Vec<&str> = Vec::new();
    loop {
        if let Ok(canon) = ancestor.canonicalize() {
            if root_missing {
                return Ok(joined.clone());
            }
            let rc = root_canon.as_deref().unwrap();
            if canon == rc || canon.starts_with(rc) {
                let mut out = canon;
                let mut parts = tail.clone();
                parts.reverse();
                for p in parts {
                    if p == ".." {
                        return Err(PathError::Escapes);
                    }
                    out.push(p);
                }
                // Final lexical guard against `..` smuggled past the ancestor.
                let mut clean = PathBuf::new();
                for c in out.components() {
                    match c {
                        Component::ParentDir => {
                            return Err(PathError::Escapes);
                        }
                        Component::CurDir => {}
                        other => clean.push(other.as_os_str()),
                    }
                }
                return Ok(clean);
            }
            return Err(PathError::Escapes);
        }
        match ancestor.file_name().and_then(|n| n.to_str()) {
            Some(name) => {
                tail.push(name);
                ancestor = ancestor.parent().unwrap_or(Path::new(""));
                if ancestor.as_os_str().is_empty() {
                    return Err(PathError::Escapes);
                }
            }
            None => return Err(PathError::Escapes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn traversal_variants_rejected() {
        let r = root();
        let root = r.path().canonicalize().unwrap();
        for bad in [
            "../x",
            "a/../../x",
            "..",
            "/etc/passwd",
            "C:/win",
            "\\\\srv\\s",
            "a\0b",
            "",
        ] {
            assert!(resolve(&root, bad, Access::Read).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn url_decoded_dots_rejected() {
        // Servers must decode %2e before resolving; decoded ".." must fail.
        let r = root();
        let root = r.path().canonicalize().unwrap();
        let decoded = "%2e%2e/x".replace("%2e", ".").replace("%2E", ".");
        assert_eq!(decoded, "../x");
        assert!(resolve(&root, &decoded, Access::Read).is_err());
    }

    #[test]
    fn symlink_escape_blocked_inside_allowed() {
        let r = root();
        let root = r.path().canonicalize().unwrap();
        // Hermetic: the outside target really exists (dangling links fail
        // closed by canonicalize, which is also safe).
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "s").unwrap();
        std::fs::write(root.join("inside.txt"), "in").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("evil.txt"))
                .unwrap();
            std::os::unix::fs::symlink(root.join("inside.txt"), root.join("ok.txt")).unwrap();
            assert!(resolve(&root, "evil.txt", Access::Read).is_err());
            assert!(resolve(&root, "ok.txt", Access::Read).is_ok());
            // Write through an escaping symlink is also blocked.
            assert!(resolve(&root, "evil.txt", Access::Write).is_err());
        }
    }

    #[test]
    fn missing_write_target_resolves_inside() {
        let r = root();
        let root = r.path().canonicalize().unwrap();
        let p = resolve(&root, "new/dir/file.rs", Access::Write).unwrap();
        assert!(p.starts_with(&root));
        assert!(resolve(&root, "../out", Access::Write).is_err());
    }

    #[test]
    fn git_dirs_protected_for_writes() {
        let r = root();
        let root = r.path().canonicalize().unwrap();
        assert_eq!(
            resolve(&root, ".git/config", Access::Write),
            Err(PathError::Protected)
        );
        // Missing targets resolve (existence is checked by the caller layer).
        assert!(resolve(&root, ".git/config", Access::Read).is_ok());
        assert!(resolve(&root, "src/main.rs", Access::Write).is_ok());
    }
}

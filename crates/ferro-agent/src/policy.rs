//! Sandbox policy: every tool is labelled read / write / destructive.
//! Writes stay under root. Destructive ops need an explicit flag.
//! `.git`, `.ferro`, `target`, `node_modules` are never writable.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Destructive,
}

#[derive(Debug, Clone)]
pub struct Sandbox {
    pub root: PathBuf,
    pub allow_write: bool,
    pub allow_destructive: bool,
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("write tools disabled (restart with --allow-write)")]
    WriteDisabled,
    #[error("destructive tools disabled (restart with --allow-destructive)")]
    DestructiveDisabled,
    #[error("path escapes workspace root")]
    EscapesRoot,
    #[error("path is protected: {0}")]
    Protected(String),
}

impl Sandbox {
    pub fn readonly(root: PathBuf) -> Self {
        Self {
            root: root.canonicalize().unwrap_or(root),
            allow_write: false,
            allow_destructive: false,
        }
    }

    pub fn check(&self, access: Access) -> Result<(), SandboxError> {
        match access {
            Access::Read => Ok(()),
            Access::Write if self.allow_write => Ok(()),
            Access::Write => Err(SandboxError::WriteDisabled),
            Access::Destructive if self.allow_write && self.allow_destructive => Ok(()),
            Access::Destructive if !self.allow_write => Err(SandboxError::WriteDisabled),
            Access::Destructive => Err(SandboxError::DestructiveDisabled),
        }
    }

    /// Resolve a workspace-relative path. Rejects traversal and protected dirs for writes.
    pub fn resolve(&self, rel: &str, access: Access) -> Result<PathBuf, SandboxError> {
        self.check(access)?;
        let joined = self.root.join(rel.trim_start_matches('/'));
        // Lexical normalize, then require prefix on canonical root.
        let mut norm = PathBuf::new();
        for c in joined.components() {
            use std::path::Component;
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    norm.pop();
                }
                other => norm.push(other.as_os_str()),
            }
        }
        if norm != self.root && !norm.starts_with(&self.root) {
            return Err(SandboxError::EscapesRoot);
        }
        if matches!(access, Access::Write | Access::Destructive) {
            let rel_norm = norm
                .strip_prefix(&self.root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            for prot in [".git", ".ferro", "target", "node_modules"] {
                if rel_norm == prot || rel_norm.starts_with(&format!("{prot}/")) {
                    return Err(SandboxError::Protected(prot.into()));
                }
            }
        }
        Ok(norm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb() -> Sandbox {
        Sandbox::readonly("/tmp/ferro-sb-test".into())
    }

    #[test]
    fn read_allowed_write_blocked() {
        let s = sb();
        assert!(s.check(Access::Read).is_ok());
        assert!(matches!(
            s.check(Access::Write),
            Err(SandboxError::WriteDisabled)
        ));
        assert!(matches!(
            s.check(Access::Destructive),
            Err(SandboxError::WriteDisabled)
        ));
    }

    #[test]
    fn destructive_needs_both_flags() {
        let mut s = sb();
        s.allow_write = true;
        assert!(matches!(
            s.check(Access::Destructive),
            Err(SandboxError::DestructiveDisabled)
        ));
        s.allow_destructive = true;
        assert!(s.check(Access::Destructive).is_ok());
    }

    #[test]
    fn traversal_rejected() {
        let s = sb();
        assert!(matches!(
            s.resolve("../../etc/passwd", Access::Read),
            Err(SandboxError::EscapesRoot)
        ));
    }

    #[test]
    fn protected_dirs_not_writable() {
        let mut s = sb();
        s.allow_write = true;
        s.allow_destructive = true;
        assert!(matches!(
            s.resolve(".git/config", Access::Write),
            Err(SandboxError::Protected(_))
        ));
        assert!(s.resolve("src/main.rs", Access::Write).is_ok());
    }
}

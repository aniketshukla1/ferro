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

    /// Resolve a workspace-relative path through `ferro_core::paths`.
    /// Keeps the extra protected list (.ferro, target, node_modules) for
    /// writes on top of the core `.git` refusal.
    pub fn resolve(&self, rel: &str, access: Access) -> Result<PathBuf, SandboxError> {
        self.check(access)?;
        // Canonical root: the core resolver returns canonical paths, so the
        // protected-dir check below must compare against the canonical root.
        let root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let core_access = match access {
            Access::Read => ferro_core::paths::Access::Read,
            Access::Write | Access::Destructive => ferro_core::paths::Access::Write,
        };
        let norm = ferro_core::paths::resolve(&root, rel, core_access).map_err(|e| match e {
            ferro_core::paths::PathError::Protected => SandboxError::Protected(".git".into()),
            _ => SandboxError::EscapesRoot,
        })?;
        if matches!(access, Access::Write | Access::Destructive) {
            let rel_norm = norm
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            for prot in [".ferro", "target", "node_modules"] {
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

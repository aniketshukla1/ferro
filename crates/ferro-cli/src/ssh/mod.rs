//! B8b `ferro ssh` helpers (parsing and release-target selection).
//! Remote orchestration lands in a follow-up commit once ORA-26 (B8a) is in `main`.

pub mod run;

use std::path::{Path, PathBuf};

use ferro_core::dirs::FerroDirs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub user: Option<String>,
    pub host: String,
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnameSm {
    pub kernel: String,
    pub machine: String,
}

/// Release triple for a cached distribution binary (matches B8 `--update` targets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseTriple {
    X86_64UnknownLinuxGnu,
    Aarch64UnknownLinuxGnu,
    X86_64UnknownLinuxMusl,
    Aarch64UnknownLinuxMusl,
    X86_64AppleDarwin,
    Aarch64AppleDarwin,
    X86_64PcWindowsMsvc,
    Aarch64PcWindowsMsvc,
    X86_64UnknownFreebsd,
}

impl ReleaseTriple {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64UnknownLinuxGnu => "x86_64-unknown-linux-gnu",
            Self::Aarch64UnknownLinuxGnu => "aarch64-unknown-linux-gnu",
            Self::X86_64UnknownLinuxMusl => "x86_64-unknown-linux-musl",
            Self::Aarch64UnknownLinuxMusl => "aarch64-unknown-linux-musl",
            Self::X86_64AppleDarwin => "x86_64-apple-darwin",
            Self::Aarch64AppleDarwin => "aarch64-apple-darwin",
            Self::X86_64PcWindowsMsvc => "x86_64-pc-windows-msvc",
            Self::Aarch64PcWindowsMsvc => "aarch64-pc-windows-msvc",
            Self::X86_64UnknownFreebsd => "x86_64-unknown-freebsd",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SshParseError {
    #[error("ssh target is empty")]
    Empty,
    #[error("ssh target missing host")]
    MissingHost,
    #[error("unsupported remote OS/arch ({kernel} {machine}): {hint}")]
    UnsupportedPlatform {
        kernel: String,
        machine: String,
        hint: &'static str,
    },
}

/// Parse `ferro ssh [user@]host[:path]` (path defaults to `.` on the remote).
pub fn parse_ssh_target(raw: &str) -> Result<SshTarget, SshParseError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(SshParseError::Empty);
    }
    let (user, host_and_path) = match raw.split_once('@') {
        Some((u, rest)) if !u.is_empty() => (Some(u.to_string()), rest),
        _ => (None, raw),
    };
    if host_and_path.is_empty() {
        return Err(SshParseError::MissingHost);
    }
    let (host, remote_path) = match host_and_path.split_once(':') {
        Some((h, p)) if !h.is_empty() => (h.to_string(), Some(p.to_string())),
        None => (host_and_path.to_string(), None),
        Some((_, _)) => return Err(SshParseError::MissingHost),
    };
    Ok(SshTarget {
        user,
        host,
        remote_path,
    })
}

/// Parse stdout from `uname -sm` (e.g. `Linux x86_64`).
pub fn parse_uname_sm(stdout: &str) -> Result<UnameSm, SshParseError> {
    let line = stdout.lines().next().unwrap_or("").trim();
    let mut parts = line.split_whitespace();
    let kernel = parts.next().unwrap_or("").to_string();
    let machine = parts.next().unwrap_or("").to_string();
    if kernel.is_empty() || machine.is_empty() {
        return Err(SshParseError::UnsupportedPlatform {
            kernel,
            machine,
            hint: "expected `uname -sm` output like `Linux x86_64`",
        });
    }
    Ok(UnameSm { kernel, machine })
}

pub fn release_triple(uname: &UnameSm) -> Result<ReleaseTriple, SshParseError> {
    let k = uname.kernel.as_str();
    let m = uname.machine.to_ascii_lowercase();
    let triple = match (k, m.as_str()) {
        ("Linux", "x86_64" | "amd64") => ReleaseTriple::X86_64UnknownLinuxGnu,
        ("Linux", "aarch64" | "arm64") => ReleaseTriple::Aarch64UnknownLinuxGnu,
        ("Darwin", "x86_64" | "amd64") => ReleaseTriple::X86_64AppleDarwin,
        ("Darwin", "arm64" | "aarch64") => ReleaseTriple::Aarch64AppleDarwin,
        ("FreeBSD", "amd64" | "x86_64") => ReleaseTriple::X86_64UnknownFreebsd,
        _ => {
            return Err(SshParseError::UnsupportedPlatform {
                kernel: uname.kernel.clone(),
                machine: uname.machine.clone(),
                hint: "ferro ssh supports common Linux, macOS, and FreeBSD amd64/arm64 hosts",
            });
        }
    };
    Ok(triple)
}

/// Local path to the cached release binary for a triple (populated by `ferro --update`).
pub fn cached_binary_path(dirs: &FerroDirs, triple: &str) -> PathBuf {
    dirs.cache_dir
        .join("binaries")
        .join(triple)
        .join(binary_name())
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "ferro.exe"
    } else {
        "ferro"
    }
}

/// Ephemeral remote install dir (under `$TMPDIR`, removed on exit).
pub fn remote_staging_dir(remote_pid: u32) -> String {
    format!(".ferro-ssh-{remote_pid}")
}

/// Remote argv must not carry the token; pass via `FERRO_TOKEN` only (AC4).
pub fn remote_launch_shell(remote_bin: &Path, remote_root: &str, token_env: &str) -> String {
    // Single-quoted paths; token is injected by the caller as an env assignment, never argv.
    format!(
        "FERRO_TOKEN={token_env} exec '{bin}' --no-open --port 0 --host 127.0.0.1 '{root}'",
        bin = remote_bin.display(),
        root = remote_root.replace('\'', "'\\''"),
        token_env = token_env,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_user_host_path() {
        let t = parse_ssh_target("dev@build.example.com:/srv/app").unwrap();
        assert_eq!(t.user.as_deref(), Some("dev"));
        assert_eq!(t.host, "build.example.com");
        assert_eq!(t.remote_path.as_deref(), Some("/srv/app"));
    }

    #[test]
    fn parse_target_host_only() {
        let t = parse_ssh_target("192.0.2.10").unwrap();
        assert!(t.user.is_none());
        assert_eq!(t.host, "192.0.2.10");
        assert!(t.remote_path.is_none());
    }

    #[test]
    fn uname_to_triple_linux_arm64() {
        let u = parse_uname_sm("Linux aarch64\n").unwrap();
        assert_eq!(
            release_triple(&u).unwrap(),
            ReleaseTriple::Aarch64UnknownLinuxGnu
        );
    }

    #[test]
    fn unknown_uname_is_clear() {
        let u = parse_uname_sm("SunOS sun4u").unwrap();
        let err = release_triple(&u).unwrap_err();
        assert!(matches!(err, SshParseError::UnsupportedPlatform { .. }));
        assert!(err.to_string().contains("SunOS"));
    }

    #[test]
    fn remote_launch_omits_token_flag() {
        let cmd = remote_launch_shell(Path::new("/tmp/.ferro-ssh-1/ferro"), ".", "$FERRO_TOKEN");
        assert!(!cmd.contains("--token"));
        assert!(cmd.contains("FERRO_TOKEN="));
    }
}

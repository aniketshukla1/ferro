//! Self-update client (B8): manifest parsing, checksum verification, archive install.
//! Network I/O stays in `ferro-cli` / `ferro-server`; this module is pure filesystem + crypto.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Every target in the release matrix (BACKEND.md B8). CI may ship a subset first.
pub const RELEASE_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-freebsd",
];

/// Env var for the update manifest URL (host is a board decision; no default URL).
pub const MANIFEST_URL_ENV: &str = "FERRO_UPDATE_MANIFEST_URL";

/// Manifest JSON cap. A real manifest is a few kilobytes.
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// Compressed release archive cap (binary budget is 20 MiB; this leaves headroom).
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// Extracted `ferro` / `ferro.exe` cap.
pub const MAX_BINARY_BYTES: usize = 64 * 1024 * 1024;

/// Whole-archive uncompressed cap (binary plus README and LICENSE).
const MAX_UNCOMPRESSED_BYTES: u64 = (MAX_BINARY_BYTES as u64) + 1024 * 1024;

const MAX_ARCHIVE_ENTRIES: u32 = 32;

/// Flat release archive name (matches `.github/workflows/release.yml`).
pub fn release_archive_name(target: &str) -> String {
    format!("ferro-{target}.tar.gz")
}

/// Installed binary name inside the flat archive for this target.
pub fn binary_name_in_archive(target: &str) -> &'static str {
    if target.contains("windows") {
        "ferro.exe"
    } else {
        "ferro"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseManifest {
    pub version: String,
    pub artifacts: std::collections::BTreeMap<String, Artifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("update URL must be https and must not carry credentials")]
    InsecureUrl,
    #[error("no artifact for target {0}")]
    MissingTarget(String),
    #[error("download empty")]
    EmptyDownload,
    #[error("download exceeds {0} bytes")]
    TooLarge(usize),
    #[error("sha256 must be 64 hex characters")]
    BadChecksum,
    #[error("checksum mismatch (expected {expected}, got {actual})")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("archive: {0}")]
    Archive(String),
    #[error("install: {0}")]
    Install(String),
    #[error("{0}")]
    Other(String),
}

/// HTTPS only, no userinfo, no whitespace. Redirect targets must pass this too.
pub fn require_https_url(url: &str) -> Result<(), UpdateError> {
    let url = url.trim();
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(UpdateError::InsecureUrl);
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(UpdateError::InsecureUrl);
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains('@') {
        return Err(UpdateError::InsecureUrl);
    }
    Ok(())
}

pub fn parse_manifest(bytes: &[u8]) -> Result<ReleaseManifest, UpdateError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UpdateError::TooLarge(MAX_MANIFEST_BYTES));
    }
    let manifest: ReleaseManifest =
        serde_json::from_slice(bytes).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    if !valid_version(&manifest.version) {
        return Err(UpdateError::Manifest(
            "version must be a semver-like token".into(),
        ));
    }
    Ok(manifest)
}

fn valid_version(v: &str) -> bool {
    if v.is_empty() || v.len() > 64 {
        return false;
    }
    let mut chars = v.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Refuse empty payloads, oversized payloads, and wrong digests before touching the installed binary.
pub fn verify_download(bytes: &[u8], expected_sha256: &str) -> Result<(), UpdateError> {
    if bytes.is_empty() {
        return Err(UpdateError::EmptyDownload);
    }
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(UpdateError::TooLarge(MAX_ARCHIVE_BYTES));
    }
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UpdateError::BadChecksum);
    }
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch { expected, actual });
    }
    Ok(())
}

/// `ferro` / `ferro.exe` at the archive root. `./ferro` (GNU tar `-C dir .`) is accepted.
/// `../ferro`, absolute paths, and nested paths are not.
fn is_root_binary(path: &Path, name: &str) -> bool {
    let mut comps = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir));
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(n)), None) => n == name,
        _ => false,
    }
}

/// Stops a gzip bomb while tar skips non-binary members.
struct ByteCap<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for ByteCap<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "uncompressed archive exceeds limit",
            ));
        }
        let max = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// Extract the ferro binary from a flat `.tar.gz` release archive (root layout).
pub fn extract_binary_from_tgz(archive: &[u8], target: &str) -> Result<Vec<u8>, UpdateError> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(UpdateError::TooLarge(MAX_ARCHIVE_BYTES));
    }
    let name = binary_name_in_archive(target);
    let dec = flate2::read::GzDecoder::new(archive);
    let capped = ByteCap {
        inner: dec,
        left: MAX_UNCOMPRESSED_BYTES,
    };
    let mut ar = tar::Archive::new(capped);
    let mut seen = 0u32;
    for entry in ar
        .entries()
        .map_err(|e| UpdateError::Archive(e.to_string()))?
    {
        seen += 1;
        if seen > MAX_ARCHIVE_ENTRIES {
            return Err(UpdateError::Archive("too many entries".into()));
        }
        let entry = entry.map_err(|e| UpdateError::Archive(e.to_string()))?;
        if entry.size() > MAX_BINARY_BYTES as u64 {
            return Err(UpdateError::TooLarge(MAX_BINARY_BYTES));
        }
        let path = entry
            .path()
            .map_err(|e| UpdateError::Archive(e.to_string()))?
            .into_owned();
        if !is_root_binary(&path, name) {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(UpdateError::Archive(format!(
                "{name} is not a regular file"
            )));
        }
        let declared = entry.size();
        if declared == 0 {
            return Err(UpdateError::EmptyDownload);
        }
        let mut buf = Vec::new();
        entry
            .take(MAX_BINARY_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| UpdateError::Archive(e.to_string()))?;
        if buf.len() > MAX_BINARY_BYTES {
            return Err(UpdateError::TooLarge(MAX_BINARY_BYTES));
        }
        if buf.is_empty() {
            return Err(UpdateError::EmptyDownload);
        }
        return Ok(buf);
    }
    Err(UpdateError::Archive(format!("{name} not found in archive")))
}

struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

/// Replace `exe_path` with verified binary bytes. The previous file is left intact on error.
pub fn install_binary(exe_path: &Path, new_binary: &[u8]) -> Result<(), UpdateError> {
    if new_binary.is_empty() {
        return Err(UpdateError::EmptyDownload);
    }
    if new_binary.len() > MAX_BINARY_BYTES {
        return Err(UpdateError::TooLarge(MAX_BINARY_BYTES));
    }
    let parent = exe_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| UpdateError::Install("no parent directory".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| UpdateError::Install(e.to_string()))?;
    let staging = parent.join(format!(".ferro-update-{}", std::process::id()));
    let _cleanup = RemoveOnDrop(&staging);
    write_durable(&staging, new_binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staging)
            .map_err(|e| UpdateError::Install(e.to_string()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staging, perms)
            .map_err(|e| UpdateError::Install(e.to_string()))?;
        // Same-directory rename replaces the destination atomically on Unix.
        // The running process keeps the old inode. Data is synced first so a
        // crash cannot leave a renamed but empty file.
        std::fs::rename(&staging, exe_path).map_err(|e| UpdateError::Install(e.to_string()))?;
    }
    #[cfg(windows)]
    {
        // Windows refuses to rename over an existing file. A running executable
        // can be renamed aside; the process keeps the old file until exit.
        // The backup name is per-process so a second update does not clobber
        // a still-locked previous image.
        let backup = parent.join(format!(".ferro-prev-{}", std::process::id()));
        if exe_path.exists() {
            std::fs::rename(exe_path, &backup).map_err(|e| UpdateError::Install(e.to_string()))?;
        }
        if let Err(e) = std::fs::rename(&staging, exe_path) {
            let _ = std::fs::rename(&backup, exe_path);
            return Err(UpdateError::Install(e.to_string()));
        }
        let _ = std::fs::remove_file(&backup);
    }
    Ok(())
}

fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let mut file = std::fs::File::create(path).map_err(|e| UpdateError::Install(e.to_string()))?;
    file.write_all(bytes)
        .map_err(|e| UpdateError::Install(e.to_string()))?;
    file.sync_all()
        .map_err(|e| UpdateError::Install(e.to_string()))?;
    Ok(())
}

/// Full apply path used by `--update` after the manifest entry is chosen.
pub fn apply_release_artifact(
    exe_path: &Path,
    target: &str,
    archive_bytes: &[u8],
    expected_sha256: &str,
) -> Result<(), UpdateError> {
    verify_download(archive_bytes, expected_sha256)?;
    let bin = extract_binary_from_tgz(archive_bytes, target)?;
    install_binary(exe_path, &bin)
}

/// Read `update.check` from a settings map (defaults to false). Callers pass user scope.
pub fn update_check_enabled(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> bool {
    values
        .get("update.check")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

const DAY_SECS: i64 = 86_400;

/// Whether a background manifest fetch should run (opt-in + at most once per UTC day).
pub fn should_run_daily_check(
    enabled: bool,
    last_check_unix_secs: Option<i64>,
    now_unix_secs: i64,
) -> bool {
    if !enabled {
        return false;
    }
    match last_check_unix_secs {
        None => true,
        Some(last) if last > now_unix_secs => true,
        Some(last) => now_unix_secs.saturating_sub(last) >= DAY_SECS,
    }
}

/// Resolve artifact for the running target triple.
pub fn artifact_for_target<'a>(
    manifest: &'a ReleaseManifest,
    target: &str,
) -> Result<&'a Artifact, UpdateError> {
    manifest
        .artifacts
        .get(target)
        .ok_or_else(|| UpdateError::MissingTarget(target.to_string()))
}

/// Compile-time target triple of this binary (musl vs gnu, msvc vs gnu, and so on).
pub fn builtin_target() -> &'static str {
    env!("FERRO_BUILD_TARGET")
}

pub fn current_target() -> String {
    match std::env::var("FERRO_UPDATE_TARGET") {
        Ok(t) if !t.is_empty() => t,
        _ => builtin_target().to_string(),
    }
}

/// Path for persisting the last background check timestamp.
pub fn daily_check_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("update-check.json")
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, Default)]
pub struct DailyCheckState {
    #[serde(default)]
    pub last_check_unix_secs: Option<i64>,
}

pub fn read_daily_check_state(path: &Path) -> DailyCheckState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return DailyCheckState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn write_daily_check_state(path: &Path, state: &DailyCheckState) -> Result<(), UpdateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| UpdateError::Other(e.to_string()))?;
    }
    let body = serde_json::to_string(state).map_err(|e| UpdateError::Other(e.to_string()))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body).map_err(|e| UpdateError::Other(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        UpdateError::Other(e.to_string())
    })?;
    Ok(())
}

/// Whether release `latest` is newer than `current` (semver: numeric major.minor.patch, then a
/// release outranks its own pre-releases, which compare dot-separated with numeric parts as numbers).
/// A leading `v` is ignored; an unparsable version is never newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<([u64; 3], Option<Vec<String>>)> {
        let v = v.trim().trim_start_matches('v');
        let v = v.split('+').next()?;
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, Some(p.split('.').map(str::to_string).collect())),
            None => (v, None),
        };
        let mut nums = core.split('.').map(|n| n.parse::<u64>().ok());
        let out = [nums.next()??, nums.next()??, nums.next()??];
        nums.next().is_none().then_some((out, pre))
    }
    let (Some((a, ap)), Some((b, bp))) = (parse(latest), parse(current)) else {
        return false;
    };
    if a != b {
        return a > b;
    }
    match (ap, bp) {
        (None, Some(_)) => true,
        (Some(x), Some(y)) => {
            for (p, q) in x.iter().zip(&y) {
                let ord = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    _ => p.cmp(q),
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord == std::cmp::Ordering::Greater;
                }
            }
            x.len() > y.len()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn newer_versions_by_semver() {
        use super::is_newer;
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.2.0", "0.2.0-rc.1"));
        assert!(!is_newer("0.2.0-rc.1", "0.2.0"));
        assert!(is_newer("0.2.0-rc.10", "0.2.0-rc.9"));
        assert!(is_newer("0.2.0-rc.1.1", "0.2.0-rc.1"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("0.2", "0.1.0"));
        assert!(is_newer("0.2.0+build.5", "0.1.0"));
    }

    use super::*;
    use std::io::Write;

    fn gz_tar(write: impl FnOnce(&mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>)) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(enc);
        write(&mut builder);
        builder.finish().unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn regular(path: &str, bytes: &[u8]) -> tar::Header {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        header
    }

    #[test]
    fn builtin_target_is_in_the_release_matrix() {
        let t = builtin_target();
        assert!(
            RELEASE_TARGETS.contains(&t),
            "compile target {t} is not in the B8 release matrix"
        );
    }

    #[test]
    fn https_urls_only() {
        assert!(require_https_url("https://example.com/manifest.json").is_ok());
        assert!(matches!(
            require_https_url("http://127.0.0.1/manifest.json"),
            Err(UpdateError::InsecureUrl)
        ));
        assert!(matches!(
            require_https_url("https://user:token@example.com/m"),
            Err(UpdateError::InsecureUrl)
        ));
        assert!(matches!(
            require_https_url("file:///tmp/m.json"),
            Err(UpdateError::InsecureUrl)
        ));
    }

    #[test]
    fn rejects_short_digest() {
        let err = verify_download(b"abc", "abcd").unwrap_err();
        assert!(matches!(err, UpdateError::BadChecksum));
    }

    #[test]
    fn extracts_dot_slash_root_binary_and_rejects_nested() {
        let good = gz_tar(|b| {
            let bytes = b"new-bin";
            let header = regular("./ferro", bytes);
            b.append(&header, &bytes[..]).unwrap();
        });
        assert_eq!(
            extract_binary_from_tgz(&good, "aarch64-apple-darwin").unwrap(),
            b"new-bin"
        );

        let nested = gz_tar(|b| {
            let bytes = b"nested";
            let header = regular("subdir/ferro", bytes);
            b.append(&header, &bytes[..]).unwrap();
        });
        let err = extract_binary_from_tgz(&nested, "aarch64-apple-darwin").unwrap_err();
        assert!(matches!(err, UpdateError::Archive(_)));
    }

    #[test]
    fn rejects_symlink_and_parent_path() {
        let link = gz_tar(|b| {
            let mut header = tar::Header::new_gnu();
            header.set_path("ferro").unwrap();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("/tmp/not-ferro").unwrap();
            header.set_size(0);
            header.set_cksum();
            b.append(&header, std::io::empty()).unwrap();
        });
        let err = extract_binary_from_tgz(&link, "x86_64-unknown-linux-gnu").unwrap_err();
        assert!(matches!(err, UpdateError::Archive(_)));

        let mut raw = tar::Header::new_gnu();
        let payload = b"pwned";
        raw.set_size(payload.len() as u64);
        raw.set_mode(0o755);
        raw.set_entry_type(tar::EntryType::Regular);
        raw.set_cksum();
        let mut block = *raw.as_bytes();
        let name = b"../ferro";
        block[..name.len()].copy_from_slice(name);
        block[name.len()] = 0;
        block[148..156].fill(b' ');
        let sum: u32 = block.iter().map(|c| u32::from(*c)).sum();
        let field = format!("{sum:06o}\0 ");
        block[148..156].copy_from_slice(field.as_bytes());
        let mut archive = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut archive, flate2::Compression::fast());
            enc.write_all(&block).unwrap();
            enc.write_all(payload).unwrap();
            let pad = (512 - (payload.len() % 512)) % 512;
            enc.write_all(&vec![0u8; pad]).unwrap();
            enc.write_all(&[0u8; 1024]).unwrap();
            enc.finish().unwrap();
        }
        let err = extract_binary_from_tgz(&archive, "x86_64-unknown-linux-gnu").unwrap_err();
        assert!(matches!(err, UpdateError::Archive(_)));
    }

    #[test]
    fn rejects_oversized_declared_binary() {
        let mut header = tar::Header::new_gnu();
        header.set_path("ferro").unwrap();
        header.set_size(MAX_BINARY_BYTES as u64 + 1);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut archive = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut archive, flate2::Compression::fast());
            enc.write_all(header.as_bytes()).unwrap();
            enc.finish().unwrap();
        }
        let err = extract_binary_from_tgz(&archive, "x86_64-unknown-linux-gnu").unwrap_err();
        assert!(matches!(err, UpdateError::TooLarge(_)));
    }

    #[test]
    fn nested_member_leaves_installed_binary() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ferro");
        std::fs::write(&exe, b"stay").unwrap();
        let nested = gz_tar(|b| {
            let bytes = b"nested";
            let header = regular("subdir/ferro", bytes);
            b.append(&header, &bytes[..]).unwrap();
        });
        let hash = sha256_hex(&nested);
        let err = apply_release_artifact(&exe, "aarch64-apple-darwin", &nested, &hash).unwrap_err();
        assert!(matches!(err, UpdateError::Archive(_)));
        assert_eq!(std::fs::read(&exe).unwrap(), b"stay");
    }

    #[test]
    fn future_last_check_runs_again() {
        assert!(should_run_daily_check(true, Some(5_000), 1_000));
        assert!(!should_run_daily_check(true, Some(1_000), 1_000 + 100));
    }
}

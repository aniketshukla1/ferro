use ferro_core::update::{
    apply_release_artifact, release_archive_name, UpdateError, RELEASE_TARGETS,
};
use std::process::Command;

fn make_release_tgz(target: &str, payload: &[u8]) -> Vec<u8> {
    let name = if target.contains("windows") {
        "ferro.exe"
    } else {
        "ferro"
    };
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join(name);
    std::fs::write(&bin_path, payload).unwrap();
    let archive = dir.path().join("out.tar.gz");
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(dir.path())
        .arg(name)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(&archive).unwrap()
}

#[test]
fn release_matrix_archive_names() {
    for target in RELEASE_TARGETS {
        assert_eq!(
            release_archive_name(target),
            format!("ferro-{target}.tar.gz")
        );
    }
}

#[test]
fn refuses_corrupted_checksum_and_keeps_binary() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("ferro");
    std::fs::write(&exe, b"original-binary").unwrap();
    let before = std::fs::read(&exe).unwrap();
    let target = std::env::var("FERRO_UPDATE_TARGET")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| ferro_core::update::builtin_target().to_string());
    let good = make_release_tgz(&target, b"new-binary-v2");
    let good_hash = ferro_core::update::sha256_hex(&good);
    let mut bad = good.clone();
    let i = bad.len() / 2;
    bad[i] ^= 0xff;
    let err = apply_release_artifact(&exe, &target, &bad, &good_hash).unwrap_err();
    assert!(matches!(err, UpdateError::ChecksumMismatch { .. }));
    assert_eq!(std::fs::read(&exe).unwrap(), before);
}

#[test]
fn refuses_empty_and_truncated_download() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("ferro");
    std::fs::write(&exe, b"keep").unwrap();
    let target = "x86_64-unknown-linux-gnu";
    let empty_err = apply_release_artifact(&exe, target, b"", "00").unwrap_err();
    assert!(matches!(empty_err, UpdateError::EmptyDownload));
    let tgz = make_release_tgz(target, b"x");
    let hash = ferro_core::update::sha256_hex(&tgz);
    let truncated = tgz[..tgz.len() / 3].to_vec();
    let trunc_err = apply_release_artifact(&exe, target, &truncated, &hash).unwrap_err();
    assert!(matches!(trunc_err, UpdateError::ChecksumMismatch { .. }));
    assert_eq!(std::fs::read(&exe).unwrap(), b"keep");
}

#[test]
fn daily_check_off_skips_fetch_plan() {
    use ferro_core::update::should_run_daily_check;
    assert!(!should_run_daily_check(false, None, 1_000_000));
    assert!(!should_run_daily_check(false, Some(0), 1_000_000));
}

#[test]
fn successful_update_replaces_binary() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("ferro");
    std::fs::write(&exe, b"old").unwrap();
    let target = "x86_64-unknown-linux-gnu";
    let payload = b"new-release";
    let tgz = make_release_tgz(target, payload);
    let hash = ferro_core::update::sha256_hex(&tgz);
    apply_release_artifact(&exe, target, &tgz, &hash).unwrap();
    assert_eq!(std::fs::read(&exe).unwrap(), payload);
}

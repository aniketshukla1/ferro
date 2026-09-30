//! B8b SSH integration tests. Full round-trip runs when a host is available.

use std::fs;
use std::path::Path;

use ferro_cli::ssh::run::{
    cleanup_remote, remote_argv_hides_token, setup_remote_bootstrap, ssh_capture,
};
use ferro_cli::ssh::{parse_uname_sm, release_triple, ReleaseTriple};

#[test]
fn fixture_uname_files_map_to_expected_triples() {
    let cases = [
        ("linux-x86_64.txt", ReleaseTriple::X86_64UnknownLinuxGnu),
        ("darwin-arm64.txt", ReleaseTriple::Aarch64AppleDarwin),
    ];
    for (file, want) in cases {
        let body = fs::read_to_string(fixture_path("uname", file)).unwrap();
        let u = parse_uname_sm(&body).unwrap();
        assert_eq!(release_triple(&u).unwrap(), want, "{file}");
    }
}

#[test]
fn fixture_unknown_uname_fails_with_message() {
    let body = fs::read_to_string(fixture_path("uname", "unknown-sunos.txt")).unwrap();
    let u = parse_uname_sm(&body).unwrap();
    let err = release_triple(&u).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("SunOS"), "{msg}");
    assert!(msg.contains("unsupported"), "{msg}");
}

/// Stages a remote ferro, asserts cleanup removes the staging dir and token is absent from `ps` argv.
///
/// ```bash
/// export FERRO_SSH_TEST='user@host'
/// export FERRO_SSH_TEST_PATH=/path/on/remote   # optional
/// cargo test -p ferro ssh_remote_cleanup -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires FERRO_SSH_TEST; QA criterion 1 may be not verified without a second host"]
fn ssh_remote_cleanup_and_ps() {
    let target = std::env::var("FERRO_SSH_TEST")
        .expect("set FERRO_SSH_TEST=user@host for a loopback or owned SSH target");
    let path = std::env::var("FERRO_SSH_TEST_PATH").unwrap_or_else(|_| ".".into());
    let full_target = if path == "." {
        target
    } else {
        format!("{target}:{path}")
    };

    let boot = setup_remote_bootstrap(&full_target).expect("remote bootstrap setup");
    let present = ssh_capture(
        &boot.dest,
        &format!("test -d {} && echo present", boot.stage),
    )
    .expect("ssh test -d");
    assert!(
        present.contains("present"),
        "staging dir should exist before cleanup"
    );

    let ps = ssh_capture(
        &boot.dest,
        "ps -eo args 2>/dev/null | grep -F '.ferro-ssh-' | grep -v grep || true",
    )
    .expect("ps");
    assert!(
        remote_argv_hides_token(&ps, &boot.session_token),
        "token or --token leaked in ps argv: {ps}"
    );

    cleanup_remote(&boot.dest, &boot.stage).expect("cleanup");
    let gone = ssh_capture(
        &boot.dest,
        &format!("test -d {} && echo present || echo gone", boot.stage),
    )
    .expect("post-cleanup check");
    assert!(
        gone.contains("gone"),
        "staging dir should be removed after cleanup, got: {gone}"
    );
}

fn fixture_path(sub: &str, file: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ssh")
        .join(sub)
        .join(file)
}

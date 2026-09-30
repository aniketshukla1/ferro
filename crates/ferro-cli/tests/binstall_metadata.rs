//! cargo-binstall must resolve the same archive names as the release workflow.

use ferro_core::update::{binary_name_in_archive, release_archive_name, RELEASE_TARGETS};
use std::path::PathBuf;

fn manifest_toml() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(root).expect("read ferro-cli Cargo.toml")
}

#[test]
fn binstall_pkg_url_uses_flat_release_archives() {
    let text = manifest_toml();
    assert!(
        text.contains("ferro-{ target }.tar.gz"),
        "pkg-url must match release.yml artifact names"
    );
    assert!(
        text.contains("bin-dir = \"{ bin }{ binary-ext }\""),
        "bin-dir must be the archive-root binary, not a directory"
    );
    assert!(text.contains("pkg-fmt = \"tgz\""));
}

fn expand_archive_name(target: &str) -> String {
    let text = manifest_toml();
    let line = text
        .lines()
        .find(|l| l.contains("pkg-url"))
        .expect("pkg-url");
    let template = line
        .split_once('=')
        .expect("pkg-url assignment")
        .1
        .trim()
        .trim_matches('"');
    assert!(
        template.starts_with("{ repo }/"),
        "pkg-url must use {{ repo }} until the board chooses a host, got {template}"
    );
    let file = template.rsplit('/').next().expect("archive file");
    file.replace("{ target }", target)
        .replace("{target}", target)
}

fn expand_bin_dir(target: &str) -> String {
    let text = manifest_toml();
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("bin-dir"))
        .expect("bin-dir");
    let template = line
        .split_once('=')
        .expect("bin-dir assignment")
        .1
        .trim()
        .trim_matches('"');
    let ext = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    template
        .replace("{ bin }", "ferro")
        .replace("{ binary-ext }", ext)
}

#[test]
fn binstall_resolves_every_release_target() {
    for target in RELEASE_TARGETS {
        assert_eq!(expand_archive_name(target), release_archive_name(target));
        assert_eq!(expand_bin_dir(target), binary_name_in_archive(target));
    }
}

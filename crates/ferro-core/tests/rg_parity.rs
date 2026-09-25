//! B2a ripgrep parity: the scan engine must report the same (file, line)
//! set as `rg --json` for 30 queries mixing literal, regex, case, word and
//! globs. `#[ignore]` — needs an `rg` binary; run explicitly:
//! `cargo test -p ferro-core --test rg_parity -- --ignored --nocapture`.
//!
//! The comparison is at (path, line) granularity: ferro returns one Hit per
//! regex match while rg returns one match per line, so multi-match lines are
//! collapsed on both sides before comparing.

use ferro_core::fileindex::FileSnapshot;
use ferro_core::scan::{Case, Mode, Query};
use std::collections::BTreeSet;
use std::process::Command;
use std::sync::atomic::AtomicBool;

const FILES: &[(&str, &str)] = &[
    (
        "src/main.rs",
        "fn main() {\n    println!(\"Hello, world!\");\n    let foo = 42;\n}\n",
    ),
    (
        "src/lib.rs",
        "pub fn serve(port: u16) {}\n// Serve the world\npub const FOO: &str = \"foo\";\n",
    ),
    (
        "src/unicode.rs",
        "// naïve café\nfn caf\u{e9}() {}\nlet emoji = \"🎉 party\";\n",
    ),
    ("src/crlf.rs", "fn crlf() {}\r\nlet x = 1;\r\n"),
    (
        "web/app.js",
        "function serve() { return 1; }\nconst SERVER = serve;\n",
    ),
    (
        "docs/readme.md",
        "# Serve\n\nRun `serve --port 80` to start.\n",
    ),
    (
        "pkg/deep/nested.go",
        "package nested\nfunc Serve() {}\n// serve serve serve\n",
    ),
    ("bin/data.bin", "not really binary in this slot\n"),
    ("long/one_line.rs", "fn big() {}\n"),
    ("empty.txt", ""),
];

/// (query, mode, case, word, include, exclude)
struct RgCase {
    pat: &'static str,
    mode: Mode,
    case: Case,
    word: bool,
    include: &'static [&'static str],
    exclude: &'static [&'static str],
}

const QUERIES: &[RgCase] = &[
    RgCase {
        pat: "serve",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "Serve",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "serve",
        mode: Mode::Literal,
        case: Case::Sensitive,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "SERVE",
        mode: Mode::Literal,
        case: Case::Insensitive,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "serve",
        mode: Mode::Literal,
        case: Case::Smart,
        word: true,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "foo",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "foo",
        mode: Mode::Literal,
        case: Case::Smart,
        word: true,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "fn",
        mode: Mode::Literal,
        case: Case::Smart,
        word: true,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "fn",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: " Caf\u{e9} ",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "🎉",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "world",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "80",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "serve",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &["src/**"],
        exclude: &[],
    },
    RgCase {
        pat: "serve",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &["**/*.go"],
        exclude: &[],
    },
    RgCase {
        pat: "serve",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &["src/**"],
    },
    RgCase {
        pat: "serve",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &["**/*.rs"],
    },
    RgCase {
        pat: r"fn \w+\(",
        mode: Mode::Regex,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: r"serve|Serve",
        mode: Mode::Regex,
        case: Case::Sensitive,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: r"SERVE",
        mode: Mode::Regex,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: r"\bfoo\b",
        mode: Mode::Regex,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: r"caf.",
        mode: Mode::Regex,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: r"port: u\d+",
        mode: Mode::Regex,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: r"^pub",
        mode: Mode::Regex,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: r"serve$",
        mode: Mode::Regex,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "main",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "nested",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "party",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "println",
        mode: Mode::Literal,
        case: Case::Smart,
        word: false,
        include: &[],
        exclude: &[],
    },
    RgCase {
        pat: "const",
        mode: Mode::Literal,
        case: Case::Smart,
        word: true,
        include: &["src/**", "web/**"],
        exclude: &["**/*.md"],
    },
];

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (p, content) in FILES {
        let full = dir.path().join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
    }
    // One real binary file (NUL in the first 8 KiB) that both sides must skip.
    std::fs::write(dir.path().join("bin/data.bin"), [0u8, 1, 2, 3, b'x']).unwrap();
    // One very long line for snippet-range parity (content still matches).
    let mut long = String::from("prefix ");
    long.push_str(&"y".repeat(3000));
    long.push_str(" needle tail\nsecond line\n");
    std::fs::write(
        dir.path().join("long/one_line.rs"),
        format!("fn big() {{}}\n{long}"),
    )
    .unwrap();
    dir
}

fn snapshot(dir: &std::path::Path) -> FileSnapshot {
    let mut paths: Vec<String> = FILES.iter().map(|(p, _)| p.to_string()).collect();
    paths.push("long/one_line.rs".into());
    paths.sort();
    paths.dedup();
    FileSnapshot {
        lower: paths.iter().map(|p| p.to_lowercase()).collect(),
        base_off: paths
            .iter()
            .map(|p| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
            .collect(),
        sizes: paths
            .iter()
            .map(|p| std::fs::metadata(dir.join(p)).map(|m| m.len()).unwrap_or(0))
            .collect(),
        mtimes: vec![0; paths.len()],
        generation: 0,
        paths,
    }
}

fn ferro_hits(snap: &FileSnapshot, dir: &std::path::Path, qi: usize) -> BTreeSet<(String, usize)> {
    let c = &QUERIES[qi];
    let mut q = Query::literal(c.pat);
    q.mode = c.mode;
    q.case = c.case;
    q.word = c.word;
    q.include = c.include.iter().map(|s| s.to_string()).collect();
    q.exclude = c.exclude.iter().map(|s| s.to_string()).collect();
    q.default_exclude = vec![];
    q.max_files = 200;
    q.max_per_file = 1000;
    let stop = AtomicBool::new(false);
    ferro_core::scan::search(snap, dir, &q, &stop)
        .unwrap()
        .files
        .into_iter()
        .flat_map(|f| f.hits.into_iter().map(move |h| (f.path.clone(), h.line)))
        .collect()
}

fn rg_hits(dir: &std::path::Path, qi: usize) -> Option<BTreeSet<(String, usize)>> {
    if Command::new("rg")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return None;
    }
    let c = &QUERIES[qi];
    let mut cmd = Command::new("rg");
    cmd.arg("--json").arg("--no-heading").arg("--with-filename");
    match c.mode {
        Mode::Literal => {
            cmd.arg("--fixed-strings");
        }
        Mode::Regex => {}
    }
    match c.case {
        Case::Smart => {
            cmd.arg("--smart-case");
        }
        Case::Insensitive => {
            cmd.arg("--ignore-case");
        }
        Case::Sensitive => {
            cmd.arg("--case-sensitive");
        }
    }
    if c.word {
        cmd.arg("--word-regexp");
    }
    for g in c.include {
        cmd.arg("--glob").arg(g);
    }
    for g in c.exclude {
        cmd.arg("--glob").arg(format!("!{g}"));
    }
    cmd.arg("--").arg(c.pat).arg(".").current_dir(dir);
    let out = cmd.output().expect("rg run");
    if !out.status.success() && out.status.code() != Some(1) {
        panic!(
            "rg failed for query {qi}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut set = BTreeSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v.get("type").and_then(|t| t.as_str()) != Some("match") {
            continue;
        }
        let path = v["data"]["path"]["text"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("./");
        let n = v["data"]["line_number"].as_u64().unwrap_or(0) as usize;
        set.insert((path.to_string(), n));
    }
    Some(set)
}

#[ignore]
#[test]
fn scan_matches_ripgrep() {
    let dir = fixture();
    let snap = snapshot(dir.path());
    let mut skipped = 0;
    for (qi, c) in QUERIES.iter().enumerate() {
        let Some(expected) = rg_hits(dir.path(), qi) else {
            skipped += 1;
            continue;
        };
        let got = ferro_hits(&snap, dir.path(), qi);
        assert_eq!(got, expected, "query {qi}: {:?} (mode {:?})", c.pat, c.mode);
    }
    if skipped > 0 {
        eprintln!("rg not installed; skipped all queries");
    } else {
        eprintln!("parity ok: {} queries", QUERIES.len());
    }
}

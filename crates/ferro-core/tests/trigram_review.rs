//! B2b review regressions: the trigram index only ever widens the scan's
//! answer — across restarts, unindexed files, case folding and damage —
//! and small changes never trigger full rebuilds.

use ferro_core::fileindex::FileIndex;
use ferro_core::trigram::{plan_query, SearchEngine, SearchIndexState};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn write(root: &Path, files: &[(&str, &[u8])]) {
    for (p, b) in files {
        let full = root.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, b).unwrap();
    }
}

/// Snapshot of every file under `root` except the index dir, path-sorted,
/// sizes and mtimes as the walk records them.
fn snapshot(root: &Path) -> FileIndex {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == "idx") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let md = p.metadata().unwrap();
            let mtime = md
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            entries.push((rel, md.len(), mtime));
        }
    }
    entries.sort();
    let idx = FileIndex::default();
    let (paths, rest): (Vec<_>, Vec<_>) = entries.into_iter().map(|(p, s, m)| (p, (s, m))).unzip();
    let (sizes, mtimes) = rest.into_iter().unzip();
    idx.store(paths, sizes, mtimes);
    idx
}

fn engine(root: &Path) -> Arc<SearchEngine> {
    SearchEngine::new(root.join("idx"))
}

fn build(e: &Arc<SearchEngine>, idx: &FileIndex, root: &Path) {
    e.ensure_built(&idx.load(), root, "on");
    let deadline = Instant::now() + Duration::from_secs(20);
    while e.state() != SearchIndexState::Ready {
        assert!(Instant::now() < deadline, "never ready: {:?}", e.state());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hits(e: &SearchEngine, idx: &FileIndex, pattern: &str, ci: bool) -> Vec<String> {
    let snap = idx.load();
    let keys = plan_query(pattern, true, ci).expect("usable trigrams");
    let c = e.candidates(&snap, &keys).expect("the index answers");
    c.into_iter().map(|i| snap.paths[i].clone()).collect()
}

#[test]
fn case_insensitive_non_ascii_keeps_matches() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &[
            ("a.txt", "CAFÉ au lait\n".as_bytes()),
            ("b.txt", "\u{212A}elvin units\n".as_bytes()),
            ("c.txt", b"other\n"),
        ],
    );
    let idx = snapshot(dir.path());
    let e = engine(dir.path());
    build(&e, &idx, dir.path());
    assert!(hits(&e, &idx, "café", true).contains(&"a.txt".to_string()));
    // KELVIN SIGN case-folds to `k`: a case-insensitive `kelvin` finds it.
    assert!(hits(&e, &idx, "kelvin", true).contains(&"b.txt".to_string()));
    assert!(!hits(&e, &idx, "café", true).contains(&"c.txt".to_string()));
}

#[test]
fn a_small_change_never_triggers_a_full_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..200 {
        write(
            dir.path(),
            &[(
                format!("f{i:03}.txt").as_str(),
                format!("file {i}\n").as_bytes(),
            )],
        );
    }
    let idx = snapshot(dir.path());
    let e = engine(dir.path());
    build(&e, &idx, dir.path());
    // One file saved: the watcher applies it to the snapshot and notes it.
    write(dir.path(), &[("f000.txt", b"file 0 zyxwvu edited\n")]);
    idx.apply_delta(&[("f000.txt".into(), 21, 0)], &[]);
    e.note_changes(&["f000.txt".into()], &[]);
    e.ensure_built(&idx.load(), dir.path(), "on");
    assert_eq!(e.state(), SearchIndexState::Ready);
    // No new build was started: exactly one published build exists and it
    // is still the first one (the edit is answered through the delta).
    std::thread::sleep(Duration::from_millis(200));
    let builds: Vec<_> = std::fs::read_dir(dir.path().join("idx"))
        .unwrap()
        .flatten()
        .filter(|x| x.file_name().to_string_lossy().starts_with("g-"))
        .collect();
    assert_eq!(builds.len(), 1);
    assert!(hits(&e, &idx, "zyxwvu", false).contains(&"f000.txt".to_string()));
}

#[test]
fn a_restart_reconciles_files_changed_while_down() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("a.txt", b"alpha\n"), ("b.txt", b"beta\n")]);
    let idx = snapshot(dir.path());
    let e = engine(dir.path());
    build(&e, &idx, dir.path());
    drop(e);
    // While ferro is down: b.txt changes, c.txt appears (a git checkout).
    std::thread::sleep(Duration::from_millis(1100));
    write(
        dir.path(),
        &[("b.txt", b"beta zyxwvu\n"), ("c.txt", b"zyxwvu too\n")],
    );
    let idx2 = snapshot(dir.path());
    let e2 = engine(dir.path());
    e2.ensure_built(&idx2.load(), dir.path(), "on");
    assert_eq!(e2.state(), SearchIndexState::Ready, "warm start");
    let h = hits(&e2, &idx2, "zyxwvu", false);
    assert!(h.contains(&"b.txt".to_string()), "{h:?}");
    assert!(h.contains(&"c.txt".to_string()), "{h:?}");
    assert!(!h.contains(&"a.txt".to_string()), "{h:?}");
}

#[test]
fn files_the_index_cannot_cover_are_always_scanned() {
    let dir = tempfile::tempdir().unwrap();
    let mut big = vec![b'x'; 9 * 1024 * 1024];
    big.extend_from_slice(b"\nneedlezz\n");
    write(
        dir.path(),
        &[("big.log", big.as_slice()), ("a.txt", b"small\n")],
    );
    let idx = snapshot(dir.path());
    let e = engine(dir.path());
    build(&e, &idx, dir.path());
    // Over the 8 MiB index cap: no trigrams, so every query includes it
    // (the scan applies search.maxFileBytes itself).
    assert!(hits(&e, &idx, "needlezz", false).contains(&"big.log".to_string()));
    assert!(hits(&e, &idx, "qqqqqq", false).contains(&"big.log".to_string()));
}

#[test]
fn a_damaged_index_is_rebuilt_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("a.txt", b"alpha beta\n")]);
    let idx = snapshot(dir.path());
    let e = engine(dir.path());
    build(&e, &idx, dir.path());
    drop(e);
    // Truncate every file of the published build.
    let cur = std::fs::read_to_string(dir.path().join("idx/current")).unwrap();
    for f in ["docs.bin", "lexicon.bin", "postings.bin"] {
        let p = dir.path().join("idx").join(cur.trim()).join(f);
        let b = std::fs::read(&p).unwrap();
        std::fs::write(&p, &b[..b.len() / 2]).unwrap();
    }
    let e2 = engine(dir.path());
    // No panic; the damaged build is ignored and a fresh one answers.
    build(&e2, &idx, dir.path());
    assert!(hits(&e2, &idx, "alpha", false).contains(&"a.txt".to_string()));
}

#[test]
fn a_crashed_writer_never_hides_the_published_build() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &[("a.txt", b"alpha beta\n")]);
    let idx = snapshot(dir.path());
    let e = engine(dir.path());
    build(&e, &idx, dir.path());
    drop(e);
    // A writer died mid-build: half a temp dir, never published.
    let tmp = dir.path().join("idx/.g-dead.tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("docs.bin"), b"\x05\x00").unwrap();
    let e2 = engine(dir.path());
    e2.ensure_built(&idx.load(), dir.path(), "on");
    assert_eq!(e2.state(), SearchIndexState::Ready);
    assert!(hits(&e2, &idx, "alpha", false).contains(&"a.txt".to_string()));
}

#[test]
fn concurrent_policy_checks_start_one_build() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..300 {
        write(
            dir.path(),
            &[(
                format!("f{i:03}.txt").as_str(),
                format!("file {i}\n").as_bytes(),
            )],
        );
    }
    let idx = Arc::new(snapshot(dir.path()));
    let e = engine(dir.path());
    let root = dir.path().to_path_buf();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (e, idx, root) = (e.clone(), idx.clone(), root.clone());
            std::thread::spawn(move || e.ensure_built(&idx.load(), &root, "on"))
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while e.state() != SearchIndexState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    // One writer: no competing builds left behind, and the index answers.
    let names: Vec<String> = std::fs::read_dir(dir.path().join("idx"))
        .unwrap()
        .flatten()
        .map(|x| x.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.starts_with("g-")).count(),
        1,
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
    assert!(hits(&e, &idx, "file 12", false).contains(&"f012.txt".to_string()));
}

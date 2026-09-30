//! Golden fuzzy ranking cases (BACKEND.md B2a, API.md § 5.1 contract).
//! Exact basename first, tight clusters, shallow ties, boost, empty query.

use ferro_core::fileindex::FileSnapshot;
use ferro_core::fuzzy::rank_snap;
use std::collections::HashSet;

fn snap(paths: &[&str]) -> FileSnapshot {
    let v: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
    FileSnapshot {
        lower: v.iter().map(|p| p.to_lowercase()).collect(),
        base_off: v
            .iter()
            .map(|p| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
            .collect(),
        sizes: vec![0; v.len()],
        mtimes: vec![0; v.len()],
        generation: 0,
        paths: v,
    }
}

fn top(paths: &[&str], q: &str) -> String {
    let s = snap(paths);
    let r = rank_snap(&s, q, 3, &HashSet::new());
    assert!(!r.is_empty(), "no match for {q}");
    s.paths[r[0].index].clone()
}

#[test]
fn kubernetes_goldens() {
    assert_eq!(
        top(
            &[
                "src/kubelet.go",
                "src/kubelet/kubelet.go",
                "pkg/kubelet.go.bak"
            ],
            "kubelet.go"
        ),
        "src/kubelet.go"
    );
    assert_eq!(
        top(
            &[
                "pkg/scheduler/scheduler.go",
                "pkg/scheduler/eventhandlers.go"
            ],
            "schdlr"
        ),
        "pkg/scheduler/scheduler.go"
    );
    assert_eq!(
        top(
            &[
                "a/cmd/kube-apiserver/apiserver.go",
                "cmd/kube-apiserver/apiserver.go"
            ],
            "apiserver"
        ),
        "cmd/kube-apiserver/apiserver.go"
    );
}

#[test]
fn empty_query_returns_nothing() {
    let s = snap(&["a.rs"]);
    assert!(rank_snap(&s, "", 5, &HashSet::new()).is_empty());
}

#[test]
fn limit_respected() {
    let paths: Vec<String> = (0..50).map(|i| format!("src/f{i:02}.rs")).collect();
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let s = snap(&refs);
    assert_eq!(rank_snap(&s, "f", 10, &HashSet::new()).len(), 10);
}

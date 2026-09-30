//! B2a criterion benches: fuzzy over 100k paths, scan over 100 MB synthetic.
//! Run: `cargo bench -p ferro-core --bench engines`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ferro_core::fileindex::FileSnapshot;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

fn big_snapshot(n: usize) -> FileSnapshot {
    // Mix of depths, extensions and name shapes (k8s-flavored).
    let dirs = [
        "pkg/scheduler",
        "pkg/kubelet",
        "cmd/kube-apiserver",
        "staging/src/k8s.io/api/core",
        "vendor/k8s.io/client-go/util",
    ];
    let mut paths = Vec::with_capacity(n);
    for i in 0..n {
        let d = dirs[i % dirs.len()];
        let name = match i % 5 {
            0 => format!("handler_{i:05}.go"),
            1 => format!("kubelet_{i:05}.go"),
            2 => format!("apiserver-config-{i:05}.go"),
            3 => format!("reconcile_verbose_{i:05}.go"),
            _ => format!("util_{i:05}.go"),
        };
        paths.push(format!("{d}/{name}"));
    }
    FileSnapshot {
        lower: paths.iter().map(|p| p.to_lowercase()).collect(),
        base_off: paths
            .iter()
            .map(|p| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
            .collect(),
        sizes: vec![1024; n],
        mtimes: vec![0; n],
        generation: 1,
        paths,
    }
}

fn bench_fuzzy(c: &mut Criterion) {
    let snap = big_snapshot(100_000);
    let boost = HashSet::new();
    c.bench_function("fuzzy/srv_100k", |b| {
        b.iter(|| {
            ferro_core::fuzzy::rank_snap(
                black_box(&snap),
                black_box("srv"),
                black_box(100),
                black_box(&boost),
            )
        })
    });
    c.bench_function("fuzzy/scheduler_100k", |b| {
        b.iter(|| {
            ferro_core::fuzzy::rank_snap(
                black_box(&snap),
                black_box("schdlr"),
                black_box(50),
                black_box(&boost),
            )
        })
    });
}

fn bench_scan(c: &mut Criterion) {
    // ~100 MB across 500 files of 200 KiB with scattered hits.
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    let chunk =
        "fn handle_request() -> Result<()> {\n    let x = compute_value(42);\n    Ok(())\n}\n";
    for i in 0..500 {
        let p = format!("src/mod_{i:03}.rs");
        let mut content = String::new();
        while content.len() < 200 * 1024 {
            content.push_str(chunk);
        }
        content.push_str("zzqqxx_no_such_token never appears\n");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join(&p), &content).unwrap();
        paths.push(p);
    }
    paths.sort();
    let snap = FileSnapshot {
        lower: paths.iter().map(|p| p.to_lowercase()).collect(),
        base_off: paths
            .iter()
            .map(|p| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
            .collect(),
        sizes: paths
            .iter()
            .map(|p| {
                std::fs::metadata(dir.path().join(p))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .collect(),
        mtimes: vec![0; paths.len()],
        generation: 1,
        paths,
    };
    let root = dir.path().to_path_buf();
    // Leak the tempdir so the files outlive the benchmark run.
    Box::leak(Box::new(dir));
    let stop = AtomicBool::new(false);
    c.bench_function("scan/miss_100MB", |b| {
        b.iter(|| {
            let q = ferro_core::scan::Query::literal("zzqqxx_no_such_token_xyz");
            ferro_core::scan::search(
                black_box(&snap),
                black_box(&root),
                black_box(&q),
                black_box(&stop),
            )
        })
    });
    c.bench_function("scan/hit_100MB", |b| {
        b.iter(|| {
            let q = ferro_core::scan::Query::literal("compute_value");
            ferro_core::scan::search(
                black_box(&snap),
                black_box(&root),
                black_box(&q),
                black_box(&stop),
            )
        })
    });
}

criterion_group!(benches, bench_fuzzy, bench_scan);
criterion_main!(benches);

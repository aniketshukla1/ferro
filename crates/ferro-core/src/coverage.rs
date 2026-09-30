//! Coverage of a change (API.md § 16.4): read the coverage report the project already produces
//! (lcov, Cobertura XML, Go coverprofile) and say which added lines ran. ferro never runs the
//! coverage command itself; it only reads the newest report it finds.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Per file (workspace-relative): lines that ran and lines that did not.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FileCov {
    pub hit: BTreeSet<u32>,
    pub miss: BTreeSet<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub path: String,
    pub format: &'static str,
    #[serde(rename = "mtimeMs")]
    pub mtime_ms: u64,
}

/// Where coverage tools write their reports, most specific first.
const CANDIDATES: [(&str, &str); 14] = [
    ("lcov.info", "lcov"),
    ("coverage/lcov.info", "lcov"),
    ("coverage.lcov", "lcov"),
    ("target/llvm-cov/lcov.info", "lcov"),
    ("target/coverage/lcov.info", "lcov"),
    ("target/lcov.info", "lcov"),
    ("coverage/lcov-report/lcov.info", "lcov"),
    ("coverage.xml", "cobertura"),
    ("coverage/cobertura-coverage.xml", "cobertura"),
    ("cobertura.xml", "cobertura"),
    ("target/cobertura.xml", "cobertura"),
    ("coverage.out", "go"),
    ("cover.out", "go"),
    ("c.out", "go"),
];

fn mtime_ms(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The newest report under `root` (also one directory down, for packages in a monorepo).
pub fn find_report(root: &Path) -> Option<Report> {
    let mut dirs = vec![String::new()];
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if e.file_type().is_ok_and(|t| t.is_dir())
                && !n.starts_with('.')
                && !["node_modules", "target", "vendor", ".git"].contains(&n.as_str())
            {
                dirs.push(n);
            }
        }
    }
    let mut best: Option<Report> = None;
    for d in &dirs {
        for (rel, format) in CANDIDATES {
            let rel = if d.is_empty() {
                rel.to_string()
            } else {
                format!("{d}/{rel}")
            };
            let full = root.join(&rel);
            if !full.is_file() {
                continue;
            }
            let m = mtime_ms(&full);
            if best.as_ref().is_none_or(|b| m > b.mtime_ms) {
                best = Some(Report {
                    path: rel,
                    format,
                    mtime_ms: m,
                });
            }
        }
    }
    best
}

/// A report path (absolute, or relative to the root or the report's folder) as workspace-relative.
fn rel_path(
    root: &Path,
    report_dir: &Path,
    p: &str,
    known: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let p = p.trim().replace('\\', "/");
    let root_s = root.to_string_lossy().replace('\\', "/");
    if let Some(r) = p.strip_prefix(&format!("{root_s}/")) {
        return Some(r.to_string());
    }
    if Path::new(&p).is_absolute() {
        // Built elsewhere (CI, a container): keep the longest suffix that exists here.
        let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        for i in 0..parts.len() {
            let cand = parts[i..].join("/");
            if known(&cand) {
                return Some(cand);
            }
        }
        return None;
    }
    let p = p.trim_start_matches("./");
    if known(p) {
        return Some(p.to_string());
    }
    let via_dir = report_dir.join(p);
    via_dir
        .strip_prefix(root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .filter(|r| known(r))
}

fn parse_lcov(text: &str, map_path: &dyn Fn(&str) -> Option<String>) -> BTreeMap<String, FileCov> {
    let mut out: BTreeMap<String, FileCov> = BTreeMap::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("SF:") {
            cur = map_path(p);
        } else if let (Some(f), Some(da)) = (&cur, line.strip_prefix("DA:")) {
            let mut it = da.split(',');
            if let (Some(n), Some(h)) = (it.next(), it.next()) {
                if let (Ok(n), Ok(h)) = (n.trim().parse::<u32>(), h.trim().parse::<i64>()) {
                    let fc = out.entry(f.clone()).or_default();
                    if h > 0 {
                        fc.hit.insert(n);
                        fc.miss.remove(&n);
                    } else if !fc.hit.contains(&n) {
                        fc.miss.insert(n);
                    }
                }
            }
        } else if line == "end_of_record" {
            cur = None;
        }
    }
    out
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    let end = tag[at..].find('"')?;
    Some(&tag[at..at + end])
}

fn parse_cobertura(
    text: &str,
    map_path: &dyn Fn(&str) -> Option<String>,
    sources: &[String],
) -> BTreeMap<String, FileCov> {
    let mut out: BTreeMap<String, FileCov> = BTreeMap::new();
    let mut cur: Option<String> = None;
    for tag in text.split('<').skip(1) {
        if tag.starts_with("class ") {
            cur = attr(tag, "filename").and_then(|f| {
                map_path(f).or_else(|| {
                    sources
                        .iter()
                        .find_map(|s| map_path(&format!("{}/{f}", s.trim_end_matches('/'))))
                })
            });
        } else if tag.starts_with("/class") {
            cur = None;
        } else if let (Some(f), true) = (&cur, tag.starts_with("line ")) {
            if let (Some(n), Some(h)) = (attr(tag, "number"), attr(tag, "hits")) {
                if let (Ok(n), Ok(h)) = (n.parse::<u32>(), h.parse::<i64>()) {
                    let fc = out.entry(f.clone()).or_default();
                    if h > 0 {
                        fc.hit.insert(n);
                        fc.miss.remove(&n);
                    } else if !fc.hit.contains(&n) {
                        fc.miss.insert(n);
                    }
                }
            }
        }
    }
    out
}

fn parse_go(
    text: &str,
    module: &str,
    map_path: &dyn Fn(&str) -> Option<String>,
) -> BTreeMap<String, FileCov> {
    let mut out: BTreeMap<String, FileCov> = BTreeMap::new();
    for line in text.lines().skip_while(|l| l.starts_with("mode:")) {
        // path/to/file.go:12.5,14.2 3 1
        let Some((loc, rest)) = line.split_once(' ') else {
            continue;
        };
        let Some((file, range)) = loc.rsplit_once(':') else {
            continue;
        };
        let count: i64 = rest
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let Some((a, b)) = range.split_once(',') else {
            continue;
        };
        let (Some(start), Some(end)) = (
            a.split('.').next().and_then(|x| x.parse::<u32>().ok()),
            b.split('.').next().and_then(|x| x.parse::<u32>().ok()),
        ) else {
            continue;
        };
        let local = file.strip_prefix(&format!("{module}/")).unwrap_or(file);
        let Some(f) = map_path(local) else { continue };
        let fc = out.entry(f).or_default();
        for n in start..=end.min(start + 500) {
            if count > 0 {
                fc.hit.insert(n);
                fc.miss.remove(&n);
            } else if !fc.hit.contains(&n) {
                fc.miss.insert(n);
            }
        }
    }
    out
}

/// Parse `report` into per-file line coverage keyed by workspace-relative path.
pub fn load(root: &Path, report: &Report) -> BTreeMap<String, FileCov> {
    let full = root.join(&report.path);
    if std::fs::metadata(&full)
        .map(|m| m.len() > 256 * 1024 * 1024)
        .unwrap_or(true)
    {
        return BTreeMap::new();
    }
    let Ok(text) = std::fs::read_to_string(&full) else {
        return BTreeMap::new();
    };
    let report_dir = full.parent().unwrap_or(root).to_path_buf();
    let known = |p: &str| !p.is_empty() && !p.contains("..") && root.join(p).is_file();
    let map_path = |p: &str| rel_path(root, &report_dir, p, &known);
    match report.format {
        "lcov" => parse_lcov(&text, &map_path),
        "cobertura" => {
            let sources: Vec<String> = text
                .split("<source>")
                .skip(1)
                .filter_map(|s| s.split("</source>").next())
                .map(|s| {
                    let s = s.trim().replace('\\', "/");
                    let root_s = root.to_string_lossy().replace('\\', "/");
                    s.strip_prefix(&format!("{root_s}/"))
                        .map(str::to_string)
                        .unwrap_or(s)
                })
                .collect();
            parse_cobertura(&text, &map_path, &sources)
        }
        "go" => {
            let module = std::fs::read_to_string(root.join("go.mod"))
                .ok()
                .and_then(|m| {
                    m.lines()
                        .find_map(|l| l.strip_prefix("module ").map(|x| x.trim().to_string()))
                })
                .unwrap_or_default();
            parse_go(&text, &module, &map_path)
        }
        _ => BTreeMap::new(),
    }
}

/// How to produce a report, per detected runner (shown when none is found).
pub fn hints(root: &Path) -> Vec<String> {
    let mut out = vec![];
    if root.join("Cargo.toml").is_file() {
        out.push("cargo llvm-cov --lcov --output-path lcov.info".into());
    }
    if root.join("go.mod").is_file() {
        out.push("go test -coverprofile=coverage.out ./...".into());
    }
    if let Ok(pkg) = std::fs::read_to_string(root.join("package.json")) {
        if pkg.contains("\"vitest\"") {
            out.push("npx vitest run --coverage --coverage.reporter=lcov".into());
        } else if pkg.contains("\"jest\"") {
            out.push("npx jest --coverage".into());
        }
    }
    if ["pyproject.toml", "pytest.ini", "setup.cfg"]
        .iter()
        .any(|f| root.join(f).is_file())
    {
        out.push("python -m pytest --cov --cov-report=lcov".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let f = d.path().join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        d
    }

    #[test]
    fn lcov_with_absolute_foreign_and_relative_paths() {
        let d = tree(&[("src/a.rs", ""), ("src/b.rs", "")]);
        let root = d.path().to_string_lossy().to_string();
        let lcov = format!("SF:{root}/src/a.rs\nDA:1,3\nDA:2,0\nend_of_record\nSF:/ci/build/checkout/src/b.rs\nDA:5,0\nDA:5,1\nend_of_record\nSF:gone.rs\nDA:1,1\nend_of_record\n");
        std::fs::write(d.path().join("lcov.info"), lcov).unwrap();
        let r = find_report(d.path()).unwrap();
        assert_eq!((r.path.as_str(), r.format), ("lcov.info", "lcov"));
        let m = load(d.path(), &r);
        assert_eq!(m["src/a.rs"].hit, [1].into());
        assert_eq!(m["src/a.rs"].miss, [2].into());
        assert_eq!(
            m["src/b.rs"].hit,
            [5].into(),
            "a line hit by any test counts as hit"
        );
        assert!(!m.contains_key("gone.rs"));
    }

    #[test]
    fn cobertura_and_go() {
        let d = tree(&[
            ("app/svc.py", ""),
            ("pkg/x/x.go", ""),
            ("go.mod", "module example.com/m\n"),
        ]);
        let xml = r#"<?xml version="1.0"?><coverage><sources><source>app</source></sources><packages><package><classes><class name="svc" filename="svc.py"><lines><line number="3" hits="1"/><line number="4" hits="0"/></lines></class></classes></package></packages></coverage>"#;
        std::fs::write(d.path().join("coverage.xml"), xml).unwrap();
        let m = load(
            d.path(),
            &Report {
                path: "coverage.xml".into(),
                format: "cobertura",
                mtime_ms: 0,
            },
        );
        assert_eq!(m["app/svc.py"].hit, [3].into());
        assert_eq!(m["app/svc.py"].miss, [4].into());
        std::fs::write(d.path().join("coverage.out"), "mode: set\nexample.com/m/pkg/x/x.go:10.2,12.3 2 1\nexample.com/m/pkg/x/x.go:14.2,14.9 1 0\n").unwrap();
        let m = load(
            d.path(),
            &Report {
                path: "coverage.out".into(),
                format: "go",
                mtime_ms: 0,
            },
        );
        assert_eq!(m["pkg/x/x.go"].hit, [10, 11, 12].into());
        assert_eq!(m["pkg/x/x.go"].miss, [14].into());
    }
}

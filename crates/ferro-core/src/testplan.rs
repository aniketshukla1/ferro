//! Affected tests (API.md § 16.2): which test command covers a set of changed files, and what
//! its output says. Detection only reads manifests; it never runs anything.
//!
//! - Rust: the Cargo packages owning the changed files, plus packages that depend on them by
//!   path (`cargo test -p a -p b`, or `--workspace` when a root manifest changed).
//! - Go: the packages (directories) with changed files (`go test -json ./dir …`).
//! - JavaScript / TypeScript: vitest `related` or jest `--findRelatedTests` for the changed files
//!   when the package uses them, else the package's `test` script.
//! - Python: pytest on the changed test files and the tests that import a changed module.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TestStep {
    pub runner: &'static str,
    pub argv: Vec<String>,
    /// Working directory relative to the workspace root ("" = the root).
    pub cwd: String,
    /// Why this step covers the change, in plain words.
    pub reason: String,
}

const SKIP_DIRS: [&str; 8] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "vendor",
];

fn read(root: &Path, rel: &str) -> Option<String> {
    let p = root.join(rel);
    if std::fs::metadata(&p).ok()?.len() > 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(p).ok()
}

fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Nearest directory (from `rel`'s own, up to the root) holding `file`.
fn nearest(root: &Path, rel: &str, file: &str) -> Option<String> {
    let mut dir = parent(rel).to_string();
    loop {
        if root.join(join(&dir, file)).is_file() {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent(&dir).to_string();
    }
}

/// Every file named `name` under `root` (manifests), depth-limited, build dirs skipped.
fn find_files(root: &Path, name: &str, max_depth: usize) -> Vec<String> {
    fn walk(root: &Path, dir: &str, name: &str, depth: usize, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(root.join(dir)) else {
            return;
        };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let rel = join(dir, &n);
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth > 0 && !SKIP_DIRS.contains(&n.as_str()) && !n.starts_with('.') {
                walk(root, &rel, name, depth - 1, out);
            } else if ft.is_file() && n == name {
                out.push(rel);
            }
        }
    }
    let mut out = vec![];
    walk(root, "", name, max_depth, &mut out);
    out
}

// ---------- Rust ----------

/// `[package] name` and the names of its path dependencies, from a Cargo.toml (plain text scan:
/// good enough for manifests, no TOML parser needed).
fn cargo_package(text: &str) -> (Option<String>, Vec<String>) {
    let mut section = String::new();
    let mut name = None;
    let mut deps = vec![];
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        if section == "package" && name.is_none() {
            if let Some(v) = t.strip_prefix("name").map(str::trim_start) {
                if let Some(v) = v.strip_prefix('=') {
                    name = Some(v.trim().trim_matches('"').to_string());
                }
            }
        }
        let dep_table = section.ends_with("dependencies");
        if dep_table && t.contains("path") && t.contains('=') {
            if let Some((k, _)) = t.split_once('=') {
                let k = k.trim().trim_matches('"');
                if !k.is_empty() && !k.contains(' ') {
                    deps.push(k.to_string());
                }
            }
        }
        // `[dependencies.foo]` tables with `path = …` inside.
        if let Some(dep) = section
            .strip_prefix("dependencies.")
            .or_else(|| section.strip_prefix("dev-dependencies."))
        {
            if t.starts_with("path") {
                deps.push(dep.to_string());
            }
        }
    }
    (name, deps)
}

fn rust_step(root: &Path, changed: &[String]) -> Option<TestStep> {
    let rs: Vec<&String> = changed
        .iter()
        .filter(|p| {
            p.ends_with(".rs")
                || p.ends_with("Cargo.toml")
                || p.ends_with("Cargo.lock")
                || p.ends_with("build.rs")
        })
        .collect();
    if rs.is_empty() || !root.join("Cargo.toml").is_file() {
        return None;
    }
    let manifests = find_files(root, "Cargo.toml", 4);
    let mut pkgs: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new(); // name -> (dir, deps)
    for m in &manifests {
        if let (Some(name), deps) = cargo_package(&read(root, m).unwrap_or_default()) {
            pkgs.insert(name, (parent(m).to_string(), deps));
        }
    }
    let mut hit: BTreeSet<String> = BTreeSet::new();
    let mut whole = false;
    for p in rs {
        if p == "Cargo.toml" || p == "Cargo.lock" {
            whole = true;
            continue;
        }
        let Some(dir) = nearest(root, p, "Cargo.toml") else {
            continue;
        };
        match pkgs.iter().find(|(_, (d, _))| *d == dir) {
            Some((name, _)) => {
                hit.insert(name.clone());
            }
            None => whole = true, // a virtual workspace manifest
        }
    }
    // Packages depending (transitively, by path) on a changed one test it too.
    loop {
        let more: Vec<String> = pkgs
            .iter()
            .filter(|(n, (_, deps))| !hit.contains(*n) && deps.iter().any(|d| hit.contains(d)))
            .map(|(n, _)| n.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        hit.extend(more);
    }
    let mut argv = vec!["cargo".to_string(), "test".to_string()];
    let reason = if whole || hit.is_empty() || (pkgs.len() > 1 && hit.len() == pkgs.len()) {
        argv.push("--workspace".into());
        "a workspace manifest changed, or every package is affected".to_string()
    } else {
        for n in &hit {
            argv.push("-p".into());
            argv.push(n.clone());
        }
        format!(
            "{} with changed files, and the packages that depend on {}",
            if hit.len() == 1 {
                "the package"
            } else {
                "the packages"
            },
            if hit.len() == 1 { "it" } else { "them" }
        )
    };
    Some(TestStep {
        runner: "cargo",
        argv,
        cwd: String::new(),
        reason,
    })
}

// ---------- Go ----------

fn go_step(root: &Path, changed: &[String]) -> Option<TestStep> {
    let go: Vec<&String> = changed
        .iter()
        .filter(|p| p.ends_with(".go") || p.ends_with("go.mod") || p.ends_with("go.sum"))
        .collect();
    if go.is_empty() {
        return None;
    }
    let module = nearest(root, go[0], "go.mod")?;
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let mut all = false;
    for p in go {
        if p.ends_with("go.mod") || p.ends_with("go.sum") {
            all = true;
        } else if root.join(p).is_file() {
            dirs.insert(parent(p).to_string());
        }
    }
    let rel = |d: &str| {
        let r = d
            .strip_prefix(module.as_str())
            .unwrap_or(d)
            .trim_start_matches('/');
        if r.is_empty() {
            "./".to_string()
        } else {
            format!("./{r}")
        }
    };
    let mut argv = vec!["go".to_string(), "test".to_string(), "-json".to_string()];
    if all || dirs.is_empty() {
        argv.push("./...".into());
    } else {
        argv.extend(dirs.iter().map(|d| rel(d)));
    }
    Some(TestStep {
        runner: "go",
        argv,
        cwd: module,
        reason: if all {
            "go.mod changed".into()
        } else {
            "the packages with changed files".into()
        },
    })
}

// ---------- JavaScript / TypeScript ----------

fn js_step(root: &Path, changed: &[String]) -> Vec<TestStep> {
    let js: Vec<&String> = changed
        .iter()
        .filter(|p| {
            [
                ".js",
                ".jsx",
                ".ts",
                ".tsx",
                ".mjs",
                ".cjs",
                ".vue",
                ".svelte",
                "package.json",
            ]
            .iter()
            .any(|e| p.ends_with(e))
        })
        .collect();
    let mut by_pkg: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in js {
        if let Some(dir) = nearest(root, p, "package.json") {
            by_pkg.entry(dir).or_default().push(p.clone());
        }
    }
    let mut out = vec![];
    for (dir, files) in by_pkg {
        let Some(text) = read(root, &join(&dir, "package.json")) else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let has_dep = |n: &str| {
            ["dependencies", "devDependencies"]
                .iter()
                .any(|k| pkg[*k].get(n).is_some())
        };
        let script = pkg["scripts"]["test"].as_str().unwrap_or("");
        let rel: Vec<String> = files
            .iter()
            .filter(|f| !f.ends_with("package.json") && root.join(f).is_file())
            .map(|f| f.strip_prefix(&format!("{dir}/")).unwrap_or(f).to_string())
            .collect();
        let pm = if root.join(join(&dir, "pnpm-lock.yaml")).is_file() {
            "pnpm"
        } else if root.join(join(&dir, "yarn.lock")).is_file() {
            "yarn"
        } else {
            "npm"
        };
        let (runner, argv, reason): (&'static str, Vec<String>, String) =
            if has_dep("vitest") && !rel.is_empty() {
                let mut a = vec![
                    "npx".into(),
                    "vitest".into(),
                    "related".into(),
                    "--run".into(),
                ];
                a.extend(rel.iter().cloned());
                (
                    "vitest",
                    a,
                    "vitest: the tests that import the changed files".into(),
                )
            } else if has_dep("jest") && !rel.is_empty() {
                let mut a = vec![
                    "npx".into(),
                    "jest".into(),
                    "--ci".into(),
                    "--findRelatedTests".into(),
                ];
                a.extend(rel.iter().cloned());
                (
                    "jest",
                    a,
                    "jest: the tests related to the changed files".into(),
                )
            } else if !script.is_empty() && !script.contains("no test specified") {
                let a = if pm == "npm" {
                    vec!["npm".into(), "test".into()]
                } else {
                    vec![pm.into(), "test".into()]
                };
                (
                    "npm",
                    a,
                    "the package's test script (it has no related-test mode)".into(),
                )
            } else {
                continue;
            };
        out.push(TestStep {
            runner,
            argv,
            cwd: dir,
            reason,
        });
    }
    out
}

// ---------- Python ----------

fn py_step(root: &Path, changed: &[String]) -> Option<TestStep> {
    let py: Vec<&String> = changed.iter().filter(|p| p.ends_with(".py")).collect();
    if py.is_empty() {
        return None;
    }
    let marker = [
        "pytest.ini",
        "pyproject.toml",
        "setup.cfg",
        "tox.ini",
        "conftest.py",
    ]
    .iter()
    .any(|m| root.join(m).is_file());
    if !marker {
        return None;
    }
    let is_test = |p: &str| {
        let n = p.rsplit('/').next().unwrap_or(p);
        n.starts_with("test_") || n.ends_with("_test.py")
    };
    let mut files: BTreeSet<String> = py
        .iter()
        .filter(|p| is_test(p) && root.join(p).is_file())
        .map(|p| p.to_string())
        .collect();
    let modules: Vec<String> = py
        .iter()
        .filter(|p| !is_test(p))
        .filter_map(|p| {
            p.rsplit('/')
                .next()?
                .strip_suffix(".py")
                .map(str::to_string)
        })
        .filter(|m| m != "__init__" && m.len() > 1)
        .collect();
    if !modules.is_empty() {
        let mut tests = vec![];
        fn walk(root: &Path, dir: &str, out: &mut Vec<String>, budget: &mut usize) {
            let Ok(rd) = std::fs::read_dir(root.join(dir)) else {
                return;
            };
            for e in rd.flatten() {
                if *budget == 0 {
                    return;
                }
                let n = e.file_name().to_string_lossy().into_owned();
                let rel = join(dir, &n);
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() && !SKIP_DIRS.contains(&n.as_str()) && !n.starts_with('.') {
                    walk(root, &rel, out, budget);
                } else if ft.is_file()
                    && (n.starts_with("test_") || n.ends_with("_test.py"))
                    && n.ends_with(".py")
                {
                    *budget -= 1;
                    out.push(rel);
                }
            }
        }
        let mut budget = 5000;
        walk(root, "", &mut tests, &mut budget);
        for t in tests {
            let text = read(root, &t).unwrap_or_default();
            let imports = text.lines().any(|l| {
                let l = l.trim_start();
                (l.starts_with("import ") || l.starts_with("from "))
                    && modules.iter().any(|m| {
                        l.split(|c: char| !c.is_alphanumeric() && c != '_')
                            .any(|w| w == m)
                    })
            });
            if imports {
                files.insert(t);
            }
        }
    }
    let mut argv = vec![
        "python".to_string(),
        "-m".into(),
        "pytest".into(),
        "-q".into(),
        "-rfE".into(),
    ];
    let reason = if files.is_empty() {
        "no test imports the changed modules directly, so every test runs".to_string()
    } else {
        argv.extend(files.iter().cloned());
        "the changed tests and the tests that import a changed module".to_string()
    };
    Some(TestStep {
        runner: "pytest",
        argv,
        cwd: String::new(),
        reason,
    })
}

/// The steps that test `changed` (workspace-relative paths). Empty: nothing recognized.
pub fn plan(root: &Path, changed: &[String]) -> Vec<TestStep> {
    let mut out = vec![];
    out.extend(rust_step(root, changed));
    out.extend(go_step(root, changed));
    out.extend(js_step(root, changed));
    out.extend(py_step(root, changed));
    out
}

// ---------- output ----------

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Failure {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Summary {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub failures: Vec<Failure>,
}

fn num_before(s: &str, word: &str) -> u32 {
    // "12 passed" / "passed: 12"-free: the number right before `word`.
    let mut total = 0;
    let toks: Vec<&str> = s
        .split(|c: char| {
            c.is_whitespace() || c == ',' || c == ';' || c == '|' || c == '(' || c == ')'
        })
        .filter(|t| !t.is_empty())
        .collect();
    for w in toks.windows(2) {
        if w[1].starts_with(word) {
            total += w[0].parse::<u32>().unwrap_or(0);
        }
    }
    total
}

/// `path:line` in a line of output: the first token that looks like a source location.
fn location(line: &str) -> Option<(String, u32)> {
    for tok in line.split(|c: char| {
        c.is_whitespace() || c == '(' || c == ')' || c == '\'' || c == '"' || c == ','
    }) {
        let mut parts = tok.split(':');
        let (Some(p), Some(l)) = (parts.next(), parts.next()) else {
            continue;
        };
        let p = p.trim_start_matches("./");
        if p.contains('.')
            && !p.contains("//")
            && !p.starts_with('/')
            && l.chars().all(|c| c.is_ascii_digit())
            && !l.is_empty()
        {
            let ext_ok = p
                .rsplit('.')
                .next()
                .is_some_and(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric()));
            if ext_ok {
                return Some((p.to_string(), l.parse().ok()?));
            }
        }
    }
    None
}

/// Pass / fail counts and failures from a runner's output (stdout + stderr).
pub fn parse(runner: &str, out: &str) -> Summary {
    let mut s = Summary::default();
    match runner {
        "cargo" => {
            for l in out.lines() {
                let t = l.trim();
                if let Some(rest) = t.strip_prefix("test result:") {
                    s.passed += num_before(rest, "passed");
                    s.failed += num_before(rest, "failed");
                    s.skipped += num_before(rest, "ignored");
                } else if let Some(name) = t
                    .strip_prefix("test ")
                    .and_then(|r| r.strip_suffix(" ... FAILED"))
                {
                    s.failures.push(Failure {
                        name: name.to_string(),
                        ..Default::default()
                    });
                } else if let Some(rest) = t.strip_prefix("thread '") {
                    // thread 'name' (tid) panicked at src/x.rs:12:5:
                    let name = rest.split('\'').next().unwrap_or("");
                    if let Some(at) = t.split(" panicked at ").nth(1) {
                        if let Some((p, line)) = location(at.trim_end_matches(':')) {
                            if let Some(f) = s.failures.iter_mut().find(|f| {
                                f.name == name
                                    || f.name.ends_with(&format!("::{name}"))
                                    || name.ends_with(&f.name)
                            }) {
                                f.path.get_or_insert(p);
                                f.line.get_or_insert(line);
                            } else {
                                s.failures.push(Failure {
                                    name: name.to_string(),
                                    path: Some(p),
                                    line: Some(line),
                                    message: String::new(),
                                });
                            }
                        }
                    }
                }
            }
            // The message: the line after "panicked at …".
            let lines: Vec<&str> = out.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if l.contains(" panicked at ") {
                    let name = l
                        .trim()
                        .strip_prefix("thread '")
                        .and_then(|r| r.split('\'').next())
                        .unwrap_or("");
                    let msg = lines.get(i + 1).map(|m| m.trim()).unwrap_or("");
                    if let Some(f) = s.failures.iter_mut().find(|f| {
                        f.message.is_empty()
                            && (f.name == name || f.name.ends_with(&format!("::{name}")))
                    }) {
                        f.message = msg.chars().take(400).collect();
                    }
                }
            }
        }
        "go" => {
            let mut output: BTreeMap<String, String> = BTreeMap::new();
            for l in out.lines() {
                let Ok(ev) = serde_json::from_str::<serde_json::Value>(l) else {
                    continue;
                };
                let test = ev["Test"].as_str().unwrap_or("");
                match ev["Action"].as_str() {
                    Some("pass") if !test.is_empty() => s.passed += 1,
                    Some("skip") if !test.is_empty() => s.skipped += 1,
                    Some("fail") if !test.is_empty() => {
                        s.failed += 1;
                        let text = output.remove(test).unwrap_or_default();
                        let loc = text.lines().find_map(location);
                        s.failures.push(Failure {
                            name: test.to_string(),
                            path: loc.as_ref().map(|(p, _)| p.clone()),
                            line: loc.map(|(_, l)| l),
                            message: text
                                .lines()
                                .map(str::trim)
                                .filter(|x| !x.starts_with("=== ") && !x.starts_with("--- "))
                                .take(3)
                                .collect::<Vec<_>>()
                                .join(" ")
                                .chars()
                                .take(400)
                                .collect(),
                        });
                    }
                    Some("output") if !test.is_empty() => {
                        output
                            .entry(test.to_string())
                            .or_default()
                            .push_str(ev["Output"].as_str().unwrap_or(""));
                    }
                    _ => {}
                }
            }
        }
        "pytest" => {
            for l in out.lines() {
                let t = l.trim();
                if let Some(rest) = t
                    .strip_prefix("FAILED ")
                    .or_else(|| t.strip_prefix("ERROR "))
                {
                    let (id, msg) = rest.split_once(" - ").unwrap_or((rest, ""));
                    let path = id.split("::").next().map(str::to_string);
                    s.failures.push(Failure {
                        name: id.to_string(),
                        path,
                        line: None,
                        message: msg.chars().take(400).collect(),
                    });
                } else if (t.contains(" passed") || t.contains(" failed") || t.contains(" error"))
                    && t.contains(" in ")
                    && t.chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit() || c == '=')
                {
                    s.passed += num_before(t, "passed");
                    s.failed += num_before(t, "failed") + num_before(t, "error");
                    s.skipped += num_before(t, "skipped");
                }
            }
            // "tests/test_x.py:12: AssertionError" lines give the failing line.
            for l in out.lines() {
                if let Some((p, line)) = location(l.trim()) {
                    if let Some(f) = s
                        .failures
                        .iter_mut()
                        .find(|f| f.line.is_none() && f.path.as_deref() == Some(p.as_str()))
                    {
                        f.line = Some(line);
                    }
                }
            }
        }
        _ => {
            // jest / vitest / npm: the summary line, and "✕"/"×"/"FAIL" lines.
            for l in out.lines() {
                let t = l.trim();
                if t.starts_with("Tests:") || t.starts_with("Tests ") {
                    s.passed += num_before(t, "passed");
                    s.failed += num_before(t, "failed");
                    s.skipped += num_before(t, "skipped") + num_before(t, "todo");
                } else if let Some(name) = t
                    .strip_prefix("✕ ")
                    .or_else(|| t.strip_prefix("× "))
                    .or_else(|| t.strip_prefix("● "))
                {
                    if !s.failures.iter().any(|f| f.name == name) {
                        s.failures.push(Failure {
                            name: name.to_string(),
                            ..Default::default()
                        });
                    }
                }
            }
            for l in out.lines() {
                if let Some((p, line)) = location(l.trim()) {
                    if let Some(f) = s.failures.iter_mut().find(|f| f.path.is_none()) {
                        f.path = Some(p);
                        f.line = Some(line);
                    }
                }
            }
        }
    }
    if s.failed == 0 {
        s.failed = s.failures.len() as u32;
    }
    s.failures.truncate(100);
    s
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
    fn rust_packages_and_their_dependents() {
        let d = tree(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
            ("crates/core/Cargo.toml", "[package]\nname = \"core\"\n"),
            ("crates/core/src/lib.rs", ""),
            (
                "crates/server/Cargo.toml",
                "[package]\nname = \"server\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
            ),
            (
                "crates/cli/Cargo.toml",
                "[package]\nname = \"cli\"\n\n[dependencies.server]\npath = \"../server\"\n",
            ),
            ("crates/other/Cargo.toml", "[package]\nname = \"other\"\n"),
        ]);
        let p = plan(d.path(), &["crates/core/src/lib.rs".into()]);
        assert_eq!(
            p[0].argv,
            vec!["cargo", "test", "-p", "cli", "-p", "core", "-p", "server"]
        );
        let p = plan(d.path(), &["crates/other/src/x.rs".into()]);
        assert_eq!(p[0].argv, vec!["cargo", "test", "-p", "other"]);
        let p = plan(d.path(), &["Cargo.toml".into()]);
        assert_eq!(p[0].argv, vec!["cargo", "test", "--workspace"]);
        assert!(plan(d.path(), &["README.md".into()]).is_empty());
    }

    #[test]
    fn go_js_python_detection() {
        let d = tree(&[
            ("go.mod", "module x\n"),
            ("pkg/a/a.go", ""),
            (
                "web/package.json",
                r#"{"devDependencies":{"vitest":"1"},"scripts":{"test":"vitest"}}"#,
            ),
            ("web/src/util.ts", ""),
            ("pyproject.toml", ""),
            ("app/billing.py", ""),
            ("tests/test_billing.py", "from app.billing import charge\n"),
            ("tests/test_other.py", "import os\n"),
        ]);
        let p = plan(
            d.path(),
            &[
                "pkg/a/a.go".into(),
                "web/src/util.ts".into(),
                "app/billing.py".into(),
            ],
        );
        let go = p.iter().find(|s| s.runner == "go").unwrap();
        assert_eq!(go.argv, vec!["go", "test", "-json", "./pkg/a"]);
        let js = p.iter().find(|s| s.runner == "vitest").unwrap();
        assert_eq!(js.cwd, "web");
        assert_eq!(
            js.argv,
            vec!["npx", "vitest", "related", "--run", "src/util.ts"]
        );
        let py = p.iter().find(|s| s.runner == "pytest").unwrap();
        assert_eq!(py.argv.last().unwrap(), "tests/test_billing.py");
        assert!(!py.argv.iter().any(|a| a.contains("test_other")));
    }

    #[test]
    fn parses_runner_output() {
        let cargo = "running 3 tests\ntest a::ok ... ok\ntest a::bad ... FAILED\n\nthread 'a::bad' (123) panicked at crates/core/src/a.rs:42:9:\nassertion failed: x == 2\n\ntest result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured\n";
        let s = parse("cargo", cargo);
        assert_eq!((s.passed, s.failed, s.skipped), (2, 1, 1));
        assert_eq!(s.failures[0].name, "a::bad");
        assert_eq!(s.failures[0].path.as_deref(), Some("crates/core/src/a.rs"));
        assert_eq!(s.failures[0].line, Some(42));
        assert_eq!(s.failures[0].message, "assertion failed: x == 2");

        let go = [
            r#"{"Action":"run","Test":"TestA"}"#,
            r#"{"Action":"output","Test":"TestA","Output":"    a_test.go:12: want 1 got 2\n"}"#,
            r#"{"Action":"fail","Test":"TestA"}"#,
            r#"{"Action":"pass","Test":"TestB"}"#,
        ]
        .join("\n");
        let s = parse("go", &go);
        assert_eq!((s.passed, s.failed), (1, 1));
        assert_eq!(s.failures[0].line, Some(12));

        let py = "F.\n___ test_charge ___\ntests/test_billing.py:7: AssertionError\nFAILED tests/test_billing.py::test_charge - assert 1 == 2\n==== 1 failed, 1 passed in 0.12s ====\n";
        let s = parse("pytest", py);
        assert_eq!((s.passed, s.failed), (1, 1));
        assert_eq!(s.failures[0].path.as_deref(), Some("tests/test_billing.py"));
        assert_eq!(s.failures[0].line, Some(7));

        let jest = " FAIL  src/util.test.ts\n  ● util › adds\n    at Object.<anonymous> (src/util.test.ts:9:13)\nTests:       1 failed, 4 passed, 5 total\n";
        let s = parse("jest", jest);
        assert_eq!((s.passed, s.failed), (4, 1));
        assert_eq!(s.failures[0].line, Some(9));
    }
}

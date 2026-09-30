//! Checks on a change (API.md § 16): breaking-change radar, affected tests, security scan,
//! coverage. Every check takes the `base` / `target` pair of § 6.2, so it works on the working
//! tree, a commit, a comparison or a PR.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::jobs::{Job, JobState};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/checks/breaking", get(breaking))
        .route("/api/v1/checks/tests/plan", get(tests_plan))
        .route("/api/v1/checks/tests/run", post(tests_run))
        .route("/api/v1/checks/security", get(security))
        .route("/api/v1/checks/security/deep", post(security_deep))
        .route("/api/v1/checks/coverage", get(coverage))
}

// ---------- 5. coverage (§ 16.4) ----------

/// Which added lines the newest coverage report says ran. Reads only.
async fn coverage(
    State(s): State<Arc<AppState>>,
    Query(q): Query<PairQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (base, target) = pair(&q)?;
    let g = repo(&s)?;
    let root = s.ws().root.clone();
    let max_rows = s.limits.max_diff_rows;
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, ApiError> {
        let Some(report) = ferro_core::coverage::find_report(&root) else {
            return Ok(serde_json::json!({ "report": null, "hints": ferro_core::coverage::hints(&root) }));
        };
        let cov = ferro_core::coverage::load(&root, &report);
        let (files, _) = added_lines(&g, &base, &target, max_rows)?;
        let (mut added, mut covered, mut uncovered) = (0usize, 0usize, 0usize);
        let mut stale = false;
        let mut rows = vec![];
        for (path, lines) in &files {
            if ferro_core::radar::is_test_path(path) {
                continue; // tests are not what coverage measures
            }
            if target == "worktree" {
                let m = std::fs::metadata(root.join(path))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                stale |= m > report.mtime_ms;
            }
            let fc = cov.get(path.as_str());
            let (mut hit, mut miss) = (vec![], vec![]);
            for (n, _) in lines {
                match fc {
                    Some(fc) if fc.hit.contains(n) => hit.push(*n),
                    Some(fc) if fc.miss.contains(n) => miss.push(*n),
                    _ => {}
                }
            }
            if fc.is_none() && !lines.is_empty() {
                // Not in the report at all: a new file, or one the tests never loaded.
                rows.push(serde_json::json!({ "path": path, "inReport": false, "added": lines.len(), "covered": 0, "uncovered": [], "coveredLines": [] }));
                continue;
            }
            if hit.is_empty() && miss.is_empty() {
                continue; // only non-executable lines (comments, blanks, signatures)
            }
            added += hit.len() + miss.len();
            covered += hit.len();
            uncovered += miss.len();
            rows.push(serde_json::json!({ "path": path, "inReport": true, "added": hit.len() + miss.len(), "covered": hit.len(), "uncovered": miss, "coveredLines": hit }));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r["uncovered"].as_array().map_or(0, Vec::len)));
        Ok(serde_json::json!({
            "report": report,
            "stale": stale,
            "files": rows,
            "totals": { "executable": added, "covered": covered, "uncovered": uncovered },
            "percent": if added > 0 { serde_json::json!((covered as f64 * 1000.0 / added as f64).round() / 10.0) } else { serde_json::Value::Null },
        }))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "checks task failed"))??;
    Ok(Json(out))
}

// ---------- 3. security (§ 16.3) ----------

const SEC_MAX_FILES: usize = 400;

/// One changed file's added lines: `(path, [(line, text)])`.
type AddedLines = (String, Vec<(u32, String)>);

/// Added lines per changed file, from the § 6.3 diff of each file.
fn added_lines(
    g: &ferro_core::git::GitRepo,
    base: &str,
    target: &str,
    max_rows: usize,
) -> Result<(Vec<AddedLines>, usize), ApiError> {
    let cs = g.changes(base, target).map_err(|e| {
        ApiError::detail(
            ErrorCode::GitFailed,
            "could not list the changes",
            serde_json::json!({ "stderr": e.stderr() }),
        )
    })?;
    let mut out = vec![];
    let files: Vec<_> = cs
        .files
        .iter()
        .filter(|f| !f.binary)
        .take(SEC_MAX_FILES)
        .collect();
    for f in &files {
        if f.status.0 == ferro_core::git::ChangeStatus::Deleted {
            continue;
        }
        let Ok(fd) = super::git::render_diff(
            g, &f.path, base, target, 0, false, false, false, false, max_rows,
        ) else {
            continue;
        };
        let mut lines = vec![];
        for h in fd["hunks"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            for r in h["rows"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                if r["t"] == "add" {
                    if let (Some(n), Some(t)) = (r["n"].as_u64(), r["text"].as_str()) {
                        lines.push((n as u32, t.to_string()));
                    }
                }
            }
        }
        if !lines.is_empty() {
            out.push((f.path.clone(), lines));
        }
    }
    Ok((out, files.len()))
}

fn sev_rank(s: &str) -> u8 {
    match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

fn sort_findings(v: &mut [serde_json::Value]) {
    v.sort_by_key(|f| {
        (
            sev_rank(f["severity"].as_str().unwrap_or("")),
            f["path"].as_str().unwrap_or("").to_string(),
            f["line"].as_u64().unwrap_or(0),
        )
    });
}

/// Built-in scan: secrets and risky patterns on added lines. Reads only; runs by itself.
async fn security(
    State(s): State<Arc<AppState>>,
    Query(q): Query<PairQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (base, target) = pair(&q)?;
    let g = repo(&s)?;
    let max_rows = s.limits.max_diff_rows;
    let root = s.ws().root.clone();
    let s = s.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, ApiError> {
        let (files, scanned) = added_lines(&g, &base, &target, max_rows)?;
        let mut findings: Vec<serde_json::Value> = vec![];
        for (path, lines) in &files {
            let refs: Vec<(u32, &str)> = lines.iter().map(|(n, t)| (*n, t.as_str())).collect();
            // Rust unit tests live in the source file: find where its test module starts.
            let test_from = if path.ends_with(".rs") {
                let text = match target.as_str() {
                    "worktree" | "index" => side_text(&g, &root, &target, None, path),
                    rev => g
                        .blob_bytes_max(rev, path, MAX_TEXT)
                        .ok()
                        .and_then(|b| String::from_utf8(b).ok()),
                };
                text.and_then(|t| ferro_core::secscan::rust_test_start(&t))
            } else {
                None
            };
            for f in ferro_core::secscan::scan_added_with(path, &refs, test_from) {
                let mut v = serde_json::to_value(&f).unwrap_or_default();
                v["tool"] = serde_json::json!("ferro");
                findings.push(v);
            }
        }
        findings.extend(memory_file_changes(&g, &base, &target, &root));
        apply_memory(&s, &mut findings);
        sort_findings(&mut findings);
        Ok(serde_json::json!({ "scanned": scanned, "findings": findings, "tools": deep_tools() }))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "checks task failed"))??;
    Ok(Json(out))
}

/// Team review memory (§ 17): mark findings an ignore rule covers (`suppressedBy`) and count
/// the hits. They stay in the list so the reader can see what memory hid.
fn apply_memory(s: &AppState, findings: &mut [serde_json::Value]) {
    let rules = super::memory::active_rules(s);
    if rules.is_empty() {
        return;
    }
    let (team, _) = super::memory::team_rules(s);
    let team_ids: std::collections::HashSet<String> = team.into_iter().map(|r| r.id).collect();
    let mut hits = vec![];
    for f in findings.iter_mut() {
        if f["rule"].as_str().is_some_and(|r| r.starts_with("memory.")) {
            continue; // a change to the rules themselves is never hidden by them
        }
        let subject = ferro_core::memory::Subject {
            source: "security",
            rule: f["rule"].as_str(),
            category: f["category"].as_str(),
            title: f["title"].as_str().unwrap_or(""),
            path: f["path"].as_str().unwrap_or(""),
        };
        if let Some(by) = super::memory::suppressor(&rules, &team_ids, &subject) {
            if let Some(id) = by["id"].as_str() {
                hits.push(id.to_string());
            }
            f["suppressedBy"] = by;
        }
    }
    super::memory::note_hits(s, &hits);
}

/// A change to `.ferro-rules.json` that adds ignore rules hides findings from everyone: say so,
/// so reviewers look at it like code.
fn memory_file_changes(
    g: &ferro_core::git::GitRepo,
    base: &str,
    target: &str,
    root: &std::path::Path,
) -> Vec<serde_json::Value> {
    let file = ferro_core::memory::TEAM_FILE;
    let Ok(cs) = g.changes(base, target) else {
        return vec![];
    };
    if !cs.files.iter().any(|f| f.path == file) {
        return vec![];
    }
    let before: Vec<String> = g
        .blob_bytes_max(&cs.base_sha, file, 1 << 20)
        .map(|b| {
            ferro_core::memory::parse_team(&b)
                .into_iter()
                .map(|r| r.id)
                .collect()
        })
        .unwrap_or_default();
    let after = side_text(g, root, &cs.target, cs.target_sha.as_deref(), file)
        .map(|t| ferro_core::memory::parse_team(t.as_bytes()))
        .unwrap_or_default();
    after
        .into_iter()
        .filter(|r| r.kind == "ignore" && !before.contains(&r.id))
        .map(|r| {
            let what = r
                .rule
                .clone()
                .or_else(|| r.title.clone())
                .or_else(|| r.category.clone())
                .unwrap_or_default();
            let where_ = if r.paths.is_empty() { "everywhere".to_string() } else { r.paths.join(", ") };
            serde_json::json!({
                "rule": "memory.new-ignore", "category": "config", "severity": "medium", "tool": "ferro",
                "title": "New team ignore rule",
                "detail": format!("This change stops “{what}” from being reported {where_} for the whole team{}. Make sure the team agrees.", if r.reason.is_empty() { String::new() } else { format!(" (reason: {})", r.reason) }),
                "path": file, "line": 0, "excerpt": "",
            })
        })
        .collect()
}

fn on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Which deep-scan tools are installed (the UI lists them before running).
fn deep_tools() -> serde_json::Value {
    serde_json::json!(["osv-scanner", "cargo-audit", "npm", "semgrep"]
        .iter()
        .map(|t| serde_json::json!({ "name": if *t == "npm" { "npm audit" } else { t }, "installed": on_path(t).is_some() }))
        .collect::<Vec<_>>())
}

const LOCKFILES: [&str; 9] = [
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.mod",
    "requirements.txt",
    "poetry.lock",
    "Pipfile.lock",
    "Gemfile.lock",
];

fn cvss_severity(score: f64) -> &'static str {
    if score >= 9.0 {
        "critical"
    } else if score >= 7.0 {
        "high"
    } else if score >= 4.0 {
        "medium"
    } else {
        "low"
    }
}

#[allow(clippy::too_many_arguments)]
fn dep_finding(
    tool: &str,
    id: &str,
    pkg: &str,
    version: &str,
    summary: &str,
    severity: &str,
    lockfile: &str,
    url: &str,
) -> serde_json::Value {
    serde_json::json!({
        "rule": id, "category": "dependency", "severity": severity, "tool": tool,
        "title": format!("{pkg}@{version}: {}", if summary.is_empty() { id } else { summary }),
        "detail": if url.is_empty() { format!("Known vulnerability {id}. Upgrade {pkg} to a fixed version.") } else { format!("Known vulnerability {id} ({url}). Upgrade {pkg} to a fixed version.") },
        "path": lockfile, "line": 0, "excerpt": "",
    })
}

/// osv-scanner `--format json`: every vulnerability of every package, per lockfile.
fn parse_osv(v: &serde_json::Value, root: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out = vec![];
    let prefix = format!("{}/", root.display());
    for r in v["results"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let src = r["source"]["path"].as_str().unwrap_or("");
        let lock = src.strip_prefix(&prefix).unwrap_or(src);
        for p in r["packages"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let (name, ver) = (
                p["package"]["name"].as_str().unwrap_or(""),
                p["package"]["version"].as_str().unwrap_or(""),
            );
            for vuln in p["vulnerabilities"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let vid = vuln["id"].as_str().unwrap_or("");
                // A group's max CVSS score covers its aliases; fall back to the database label.
                let score = p["groups"]
                    .as_array()
                    .and_then(|g| {
                        g.iter().find(|g| {
                            g["ids"]
                                .as_array()
                                .is_some_and(|ids| ids.iter().any(|i| i == vid))
                        })
                    })
                    .and_then(|g| g["max_severity"].as_str())
                    .and_then(|s| s.parse::<f64>().ok());
                let sev = score.map(cvss_severity).unwrap_or(
                    match vuln["database_specific"]["severity"].as_str().unwrap_or("") {
                        "CRITICAL" => "critical",
                        "HIGH" => "high",
                        "LOW" => "low",
                        _ => "medium",
                    },
                );
                out.push(dep_finding(
                    "osv-scanner",
                    vid,
                    name,
                    ver,
                    vuln["summary"].as_str().unwrap_or(""),
                    sev,
                    lock,
                    &format!("https://osv.dev/{vid}"),
                ));
            }
        }
    }
    out
}

/// `npm audit --json` (npm 7+): one finding per vulnerable package.
fn parse_npm_audit(v: &serde_json::Value, lock: &str) -> Vec<serde_json::Value> {
    let mut out = vec![];
    for (pkg, x) in v["vulnerabilities"]
        .as_object()
        .cloned()
        .unwrap_or_default()
    {
        let via = x["via"]
            .as_array()
            .and_then(|a| a.iter().find(|e| e.is_object()))
            .cloned()
            .unwrap_or_default();
        let sev = match x["severity"].as_str().unwrap_or("") {
            "critical" => "critical",
            "high" => "high",
            "moderate" => "medium",
            _ => "low",
        };
        let id = via["url"]
            .as_str()
            .and_then(|u| u.rsplit('/').next())
            .unwrap_or("npm-advisory")
            .to_string();
        out.push(dep_finding(
            "npm audit",
            &id,
            &pkg,
            x["range"].as_str().unwrap_or(""),
            via["title"].as_str().unwrap_or(""),
            sev,
            lock,
            via["url"].as_str().unwrap_or(""),
        ));
    }
    out
}

/// semgrep `--json`: results on lines the change added.
fn parse_semgrep(
    v: &serde_json::Value,
    added: &std::collections::HashSet<(String, u32)>,
) -> Vec<serde_json::Value> {
    let mut out = vec![];
    for r in v["results"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let path = r["path"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("./")
            .to_string();
        let line = r["start"]["line"].as_u64().unwrap_or(0) as u32;
        if !added.contains(&(path.clone(), line)) {
            continue;
        }
        let sev = match r["extra"]["severity"].as_str().unwrap_or("") {
            "ERROR" => "high",
            "WARNING" => "medium",
            _ => "low",
        };
        let id = r["check_id"].as_str().unwrap_or("");
        out.push(serde_json::json!({
            "rule": id, "category": "code", "severity": sev, "tool": "semgrep",
            "title": id.rsplit('.').next().unwrap_or(id).replace('-', " "),
            "detail": r["extra"]["message"], "path": path, "line": line,
            "excerpt": ferro_core::text::truncate_utf8(r["extra"]["lines"].as_str().unwrap_or("").trim(), 200),
        }));
    }
    out
}

/// Deep scan: installed external tools on the change (dependency advisories for changed
/// lockfiles, semgrep on changed files, filtered to added lines). They read files only, but may
/// download advisory databases or rule packs, so this runs on request.
async fn security_deep(
    State(s): State<Arc<AppState>>,
    Json(b): Json<PairBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let (base, target) = pair(&PairQ {
        base: b.base,
        target: b.target,
    })?;
    if s.jobs.list().iter().any(|j| {
        j.kind == "checks.security" && matches!(j.state, JobState::Running | JobState::Queued)
    }) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "a security scan is already running",
        ));
    }
    let g = repo(&s)?;
    let root = s.ws().root.clone();
    let max_rows = s.limits.max_diff_rows;
    let token = tokio_util::sync::CancellationToken::new();
    let mut job = Job::new("checks.security");
    job.progress = Some(serde_json::json!({ "message": "reading the change" }));
    let job = s.jobs.register(job, token.clone());
    s.jobs.update(&job.id, |j| j.state = JobState::Running);
    publish(&s, &job.id);
    let (s2, id) = (s.clone(), job.id.clone());
    tokio::spawn(async move {
        let s3 = s2.clone();
        let s4 = s2.clone();
        let id2 = id.clone();
        let id3 = id.clone();
        let res = tokio::task::spawn_blocking(move || {
            let progress = |m: &str| {
                s3.jobs.update(&id2, |j| j.progress = Some(serde_json::json!({ "message": m })));
                publish(&s3, &id2);
            };
            let (files, _) = added_lines(&g, &base, &target, max_rows)?;
            let changed: Vec<String> = g
                .changes(&base, &target)
                .map(|cs| cs.files.into_iter().map(|f| f.path).collect())
                .unwrap_or_default();
            let lockfiles: Vec<&String> = changed
                .iter()
                .filter(|p| LOCKFILES.contains(&p.rsplit('/').next().unwrap_or(p)))
                .collect();
            let mut findings: Vec<serde_json::Value> = vec![];
            let mut tools: Vec<serde_json::Value> = vec![];
            // Tools write JSON to files: it can outgrow the 32 KiB output tail.
            let out_dir = std::env::temp_dir().join(format!("ferro-{id3}"));
            let _ = std::fs::create_dir_all(&out_dir);
            let read_json = |name: &str, fallback: &str| -> Option<serde_json::Value> {
                std::fs::read(out_dir.join(name))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .or_else(|| serde_json::from_str(fallback).ok())
            };
            let run = |argv: Vec<String>, cwd: &std::path::Path| {
                ferro_agent::harness::dispatch(&argv, cwd, std::time::Duration::from_secs(300), &token, &[("NO_COLOR".into(), "1".into())])
            };
            // Dependencies: osv-scanner covers every ecosystem; else cargo-audit / npm audit.
            if lockfiles.is_empty() {
                tools.push(serde_json::json!({ "name": "dependencies", "status": "not-needed", "detail": "no lockfile or manifest changed" }));
            } else if on_path("osv-scanner").is_some() {
                progress("osv-scanner: checking dependencies");
                let mut argv = vec!["osv-scanner".to_string(), "--format".into(), "json".into(), "--output".into(), out_dir.join("osv.json").display().to_string()];
                for l in &lockfiles {
                    argv.push("-L".into());
                    argv.push(l.to_string());
                }
                match run(argv, &root) {
                    Ok(d) => {
                        let parsed = read_json("osv.json", &d.stdout_tail);
                        let v = parsed.clone().unwrap_or_default();
                        let found = parse_osv(&v, &root);
                        let n = found.len();
                        findings.extend(found);
                        let ok = parsed.is_some() && d.exit_code <= 1;
                        tools.push(serde_json::json!({ "name": "osv-scanner", "status": if ok { "ran" } else { "failed" }, "detail": if ok { format!("{n} advisories") } else { ferro_core::text::truncate_utf8(d.stderr_tail.trim(), 300).to_string() } }));
                    }
                    Err(e) => tools.push(serde_json::json!({ "name": "osv-scanner", "status": "failed", "detail": e.to_string() })),
                }
            } else {
                for lock in &lockfiles {
                    let name = lock.rsplit('/').next().unwrap_or(lock);
                    let dir = root.join(lock.rsplit_once('/').map(|(d, _)| d).unwrap_or(""));
                    if name == "Cargo.lock" && on_path("cargo-audit").is_some() {
                        progress("cargo audit: checking Rust dependencies");
                        match run(vec!["cargo-audit".into(), "audit".into(), "--json".into()], &dir) {
                            Ok(d) => {
                                let v: serde_json::Value = serde_json::from_str(&d.stdout_tail).unwrap_or_default();
                                let list = v["vulnerabilities"]["list"].as_array().cloned().unwrap_or_default();
                                for x in &list {
                                    let a = &x["advisory"];
                                    let sev = "high";
                                    findings.push(dep_finding("cargo-audit", a["id"].as_str().unwrap_or(""), x["package"]["name"].as_str().unwrap_or(""), x["package"]["version"].as_str().unwrap_or(""), a["title"].as_str().unwrap_or(""), sev, lock, a["url"].as_str().unwrap_or("")));
                                }
                                tools.push(serde_json::json!({ "name": "cargo-audit", "status": "ran", "detail": format!("{} advisories", list.len()) }));
                            }
                            Err(e) => tools.push(serde_json::json!({ "name": "cargo-audit", "status": "failed", "detail": e.to_string() })),
                        }
                    } else if name == "package-lock.json" && on_path("npm").is_some() {
                        progress("npm audit: checking npm dependencies");
                        match run(vec!["npm".into(), "audit".into(), "--json".into(), "--package-lock-only".into()], &dir) {
                            Ok(d) => {
                                let v: serde_json::Value = serde_json::from_str(&d.stdout_tail).unwrap_or_default();
                                let found = parse_npm_audit(&v, lock);
                                let n = found.len();
                                findings.extend(found);
                                tools.push(serde_json::json!({ "name": "npm audit", "status": "ran", "detail": format!("{n} vulnerable packages") }));
                            }
                            Err(e) => tools.push(serde_json::json!({ "name": "npm audit", "status": "failed", "detail": e.to_string() })),
                        }
                    }
                }
                if tools.is_empty() {
                    tools.push(serde_json::json!({ "name": "dependencies", "status": "missing", "detail": "install osv-scanner (or cargo-audit / npm) to check dependency advisories" }));
                }
            }
            // semgrep on the changed files, only findings on added lines.
            let targets: Vec<&String> = files.iter().map(|(p, _)| p).take(200).collect();
            if on_path("semgrep").is_none() {
                tools.push(serde_json::json!({ "name": "semgrep", "status": "missing", "detail": "install semgrep for rule-based code scanning" }));
            } else if targets.is_empty() {
                tools.push(serde_json::json!({ "name": "semgrep", "status": "not-needed", "detail": "no added lines" }));
            } else {
                progress("semgrep: scanning changed files");
                let config = [".semgrep.yml", ".semgrep.yaml", ".semgrep"]
                    .iter()
                    .find(|c| root.join(c).exists())
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "p/default".into());
                let mut argv = vec!["semgrep".to_string(), "scan".into(), "--json".into(), "--metrics=off".into(), "--quiet".into(), "--config".into(), config.clone(), "--output".into(), out_dir.join("semgrep.json").display().to_string(), "--".into()];
                argv.extend(targets.iter().map(|t| t.to_string()));
                match run(argv, &root) {
                    Ok(d) => {
                        let v = read_json("semgrep.json", &d.stdout_tail).unwrap_or_default();
                        let added: std::collections::HashSet<(String, u32)> = files.iter().flat_map(|(p, ls)| ls.iter().map(move |(n, _)| (p.clone(), *n))).collect();
                        let found = parse_semgrep(&v, &added);
                        let n = found.len();
                        findings.extend(found);
                        tools.push(serde_json::json!({ "name": "semgrep", "status": if v.is_object() { "ran" } else { "failed" }, "detail": format!("{n} findings on added lines ({config})") }));
                    }
                    Err(e) => tools.push(serde_json::json!({ "name": "semgrep", "status": "failed", "detail": e.to_string() })),
                }
            }
            let _ = std::fs::remove_dir_all(&out_dir);
            apply_memory(&s4, &mut findings);
            sort_findings(&mut findings);
            Ok::<_, ApiError>(serde_json::json!({ "findings": findings, "tools": tools }))
        })
        .await;
        let cancelled = s2
            .jobs
            .get(&id)
            .is_some_and(|j| j.state == JobState::Cancelled);
        s2.jobs.update(&id, |j| {
            j.ended_at = Some(crate::jobs::now_iso());
            match res {
                Ok(Ok(v)) if !cancelled => {
                    j.state = JobState::Done;
                    j.result = Some(v);
                }
                Ok(Err(e)) => {
                    j.state = JobState::Failed;
                    j.error = Some(serde_json::json!({ "message": e.to_string() }));
                }
                _ if cancelled => {}
                _ => {
                    j.state = JobState::Failed;
                    j.error = Some(serde_json::json!({ "message": "security task failed" }));
                }
            }
        });
        publish(&s2, &id);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job": { "id": job.id, "kind": "checks.security" } })),
    ))
}

#[derive(Deserialize)]
pub(crate) struct PairQ {
    pub base: Option<String>,
    pub target: Option<String>,
}

fn repo(s: &AppState) -> Result<ferro_core::git::GitRepo, ApiError> {
    Ok(s.ws()
        .git
        .as_ref()
        .ok_or_else(|| ApiError::new(ErrorCode::Unsupported, "not a git repository"))?
        .repo
        .clone())
}

/// Paths the pair changes (new side, plus the old side of renames).
async fn changed_paths(
    g: ferro_core::git::GitRepo,
    base: String,
    target: String,
) -> Result<Vec<String>, ApiError> {
    tokio::task::spawn_blocking(move || g.changes(&base, &target))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(|e| {
            ApiError::detail(
                ErrorCode::GitFailed,
                "could not list the changes",
                serde_json::json!({ "stderr": e.stderr() }),
            )
        })
        .map(|cs| {
            let mut out: Vec<String> = cs.files.iter().map(|f| f.path.clone()).collect();
            out.extend(cs.files.iter().filter_map(|f| f.old_path.clone()));
            out
        })
}

// ---------- 2. affected tests (§ 16.2) ----------

/// `checks.runTests`: `auto` (default) runs tests for local folders but never for a PR checkout,
/// whose code is untrusted; `on`; `off`. Returns the refusal, if any.
fn tests_refusal(s: &AppState) -> Option<&'static str> {
    let ws = s.ws();
    let eff = s.settings.effective(&ws.key);
    match eff.get("checks.runTests").and_then(|v| v.as_str()).unwrap_or("auto") {
        "on" => None,
        "off" => Some("Running tests is off (Settings → Checks → Run tests)."),
        _ if matches!(ws.mode, crate::state::Mode::Pr) => Some(
            "This is a pull-request checkout: running its tests runs its code. Set Settings → Checks → Run tests to on to allow it.",
        ),
        _ => None,
    }
}

async fn tests_plan(
    State(s): State<Arc<AppState>>,
    Query(q): Query<PairQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (base, target) = pair(&q)?;
    let changed = changed_paths(repo(&s)?, base, target.clone()).await?;
    let root = s.ws().root.clone();
    let steps = tokio::task::spawn_blocking(move || ferro_core::testplan::plan(&root, &changed))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "plan task failed"))?;
    let refusal = tests_refusal(&s);
    Ok(Json(serde_json::json!({
        "steps": steps,
        "allowed": refusal.is_none(),
        "refusal": refusal,
        // Tests run against the files on disk, whatever pair selected them.
        "againstWorktree": target == "worktree",
    })))
}

#[derive(Deserialize)]
struct PairBody {
    base: Option<String>,
    target: Option<String>,
}

/// Run the plan's steps in order as one `checks.tests` job. Each step's output is parsed into
/// pass / fail counts and failures with their file and line.
async fn tests_run(
    State(s): State<Arc<AppState>>,
    Json(b): Json<PairBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    if let Some(why) = tests_refusal(&s) {
        return Err(ApiError::new(ErrorCode::Forbidden, why));
    }
    let (base, target) = pair(&PairQ {
        base: b.base,
        target: b.target,
    })?;
    let changed = changed_paths(repo(&s)?, base, target).await?;
    let root = s.ws().root.clone();
    let root2 = root.clone();
    let steps = tokio::task::spawn_blocking(move || ferro_core::testplan::plan(&root2, &changed))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "plan task failed"))?;
    if steps.is_empty() {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "no test runner recognized for these files",
        ));
    }
    if s.jobs.list().iter().any(|j| {
        j.kind == "checks.tests" && matches!(j.state, JobState::Running | JobState::Queued)
    }) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "tests are already running",
        ));
    }
    let token = tokio_util::sync::CancellationToken::new();
    let mut job = Job::new("checks.tests");
    let shown: Vec<String> = steps.iter().map(|st| st.argv.join(" ")).collect();
    job.progress = Some(
        serde_json::json!({ "steps": shown, "step": 0, "message": format!("running {}", shown[0]) }),
    );
    let job = s.jobs.register(job, token.clone());
    s.jobs.update(&job.id, |j| j.state = JobState::Running);
    publish(&s, &job.id);
    let (s2, id) = (s.clone(), job.id.clone());
    tokio::spawn(async move {
        let mut results = vec![];
        let (mut passed, mut failed, mut skipped, mut ok) = (0, 0, 0, true);
        for (i, st) in steps.iter().enumerate() {
            if token.is_cancelled() {
                break;
            }
            s2.jobs.update(&id, |j| {
                j.progress = Some(serde_json::json!({ "steps": shown, "step": i, "message": format!("running {}", shown[i]) }));
            });
            publish(&s2, &id);
            let (argv, cwd, tok) = (st.argv.clone(), root.join(&st.cwd), token.clone());
            let env: Vec<(String, String)> = [
                ("CI", "1"),
                ("NO_COLOR", "1"),
                ("CARGO_TERM_COLOR", "never"),
                ("FORCE_COLOR", "0"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
            let out = tokio::task::spawn_blocking(move || {
                ferro_agent::harness::dispatch(
                    &argv,
                    &cwd,
                    std::time::Duration::from_secs(20 * 60),
                    &tok,
                    &env,
                )
            })
            .await;
            let step = match out {
                Ok(Ok(d)) => {
                    let text = format!("{}\n{}", d.stdout_tail, d.stderr_tail);
                    let sum = ferro_core::testplan::parse(st.runner, &text);
                    passed += sum.passed;
                    failed += sum.failed;
                    skipped += sum.skipped;
                    ok &= d.exit_code == 0 && !d.timed_out && !d.cancelled;
                    let (tail, _) = ferro_agent::redact::redact_text(
                        ferro_core::text::truncate_utf8_tail(&text, 12 * 1024),
                    );
                    serde_json::json!({
                        "runner": st.runner, "command": st.argv.join(" "), "cwd": st.cwd, "reason": st.reason,
                        "exitCode": d.exit_code, "ms": d.ms, "timedOut": d.timed_out, "cancelled": d.cancelled,
                        "passed": sum.passed, "failed": sum.failed, "skipped": sum.skipped,
                        "failures": sum.failures, "outputTail": tail,
                    })
                }
                Ok(Err(e)) => {
                    ok = false;
                    serde_json::json!({ "runner": st.runner, "command": st.argv.join(" "), "cwd": st.cwd, "error": e.to_string() })
                }
                Err(_) => {
                    ok = false;
                    serde_json::json!({ "runner": st.runner, "command": st.argv.join(" "), "cwd": st.cwd, "error": "test task failed" })
                }
            };
            results.push(step);
        }
        let cancelled = token.is_cancelled();
        s2.jobs.update(&id, |j| {
            j.state = if cancelled { JobState::Cancelled } else { JobState::Done };
            j.ended_at = Some(crate::jobs::now_iso());
            j.result = Some(serde_json::json!({
                "ok": ok && !cancelled, "passed": passed, "failed": failed, "skipped": skipped, "steps": results,
            }));
        });
        publish(&s2, &id);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job": { "id": job.id, "kind": "checks.tests" } })),
    ))
}

fn publish(s: &AppState, id: &str) {
    if let Some(j) = s.jobs.get(id) {
        s.bus.publish(crate::bus::ServerEvent::Job {
            job: serde_json::json!(j),
        });
    }
}

pub(crate) fn pair(q: &PairQ) -> Result<(String, String), ApiError> {
    let base = q.base.clone().unwrap_or_else(|| "HEAD".into());
    let target = q.target.clone().unwrap_or_else(|| "worktree".into());
    if base.len() > 256 || target.len() > 256 {
        return Err(ApiError::bad_request("base/target too long"));
    }
    Ok((base, target))
}

const MAX_FILES: usize = 300;
const MAX_TEXT: u64 = 2 * 1024 * 1024;
/// Names this common hit unrelated code everywhere; their callers are not listed.
const MAX_REFS: usize = 300;

/// Text of `path` on the `target` side: the worktree file, the index, or a revision.
pub(crate) fn side_text(
    g: &ferro_core::git::GitRepo,
    root: &std::path::Path,
    target: &str,
    target_sha: Option<&str>,
    path: &str,
) -> Option<String> {
    let bytes = match target {
        "worktree" => {
            let full = ferro_core::paths::git_rel(root, path, ferro_core::paths::Access::Read)
                .ok()
                .map(|rel| root.join(rel))?;
            if std::fs::metadata(&full).ok()?.len() > MAX_TEXT {
                return None;
            }
            std::fs::read(full).ok()?
        }
        "index" => g.blob_bytes_max("", path, MAX_TEXT).ok()?,
        _ => g.blob_bytes_max(target_sha?, path, MAX_TEXT).ok()?,
    };
    String::from_utf8(bytes).ok()
}

/// Breaking-change radar (§ 16.1): definitions the change removes, renames or re-signatures,
/// with the places in the workspace that still use them.
async fn breaking(
    State(s): State<Arc<AppState>>,
    Query(q): Query<PairQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use ferro_core::git::ChangeStatus as C;
    let (base, target) = pair(&q)?;
    let ws = s.ws();
    let g = ws
        .git
        .as_ref()
        .ok_or_else(|| ApiError::new(ErrorCode::Unsupported, "not a git repository"))?
        .repo
        .clone();
    super::nav::ensure_ready(&ws).await?;
    let root = ws.root.clone();
    let ws2 = ws.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, ApiError> {
        let cs = g.changes(&base, &target).map_err(|e| {
            ApiError::detail(
                ErrorCode::GitFailed,
                "could not list the changes",
                serde_json::json!({ "stderr": e.stderr() }),
            )
        })?;
        let files: Vec<_> = cs
            .files
            .iter()
            .filter(|f| !f.binary && !matches!(f.status.0, C::Added | C::Untracked))
            .take(MAX_FILES)
            .collect();
        let changed: std::collections::HashSet<&str> =
            cs.files.iter().map(|f| f.path.as_str()).collect();
        let indexed = ws2.symbols.references_of("_", 1).is_some();
        let mut changes = vec![];
        for f in &files {
            let old_path = f.old_path.as_deref().unwrap_or(&f.path);
            let old = g
                .blob_bytes_max(&cs.base_sha, old_path, MAX_TEXT)
                .ok()
                .and_then(|b| String::from_utf8(b).ok());
            let Some(old) = old else { continue };
            let new = if f.status.0 == C::Deleted {
                None
            } else {
                side_text(&g, &root, &cs.target, cs.target_sha.as_deref(), &f.path)
            };
            for c in ferro_core::radar::api_changes(&f.path, Some(&old), new.as_deref()) {
                // Callers still using the old name (renamed / removed), or every caller of a
                // re-signatured one, from the workspace's reference index.
                let (refs, truncated) = ws2
                    .symbols
                    .references_of(&c.name, MAX_REFS + 1)
                    .unwrap_or_default();
                let common = refs.len() > MAX_REFS || truncated;
                let refs: Vec<_> = refs
                    .into_iter()
                    .filter(|r| {
                        // The definition itself is not a caller.
                        !(r.path == f.path
                            && (Some(r.line as usize) == c.new_line
                                || (c.change != "signature" && r.line as usize == c.old_line)))
                    })
                    .collect();
                let outside = refs.iter().filter(|r| r.path != f.path).count();
                // Another definition of the name elsewhere (moved, or an unrelated twin):
                // its callers may be fine.
                let still_defined = c.change != "signature"
                    && ws2
                        .symbols
                        .by_name(&c.name)
                        .iter()
                        .any(|(d, _)| d.path != f.path);
                let severity = match (c.change, refs.is_empty() || common) {
                    ("signature", false) => "medium",
                    (_, false) if !still_defined && outside > 0 => "high",
                    (_, false) => "medium",
                    (_, true) if c.public && c.change != "signature" => "low",
                    _ => "info",
                };
                let mut v = serde_json::to_value(&c).unwrap_or_default();
                v["path"] = serde_json::json!(f.path);
                v["severity"] = serde_json::json!(severity);
                v["stillDefined"] = serde_json::json!(still_defined);
                v["refs"] = serde_json::json!({
                    "count": if common { serde_json::Value::Null } else { serde_json::json!(refs.len()) },
                    "outsideFile": outside,
                    "common": common,
                    "inChangedFiles": refs.iter().filter(|r| changed.contains(r.path.as_str())).count(),
                    "sample": refs.iter().take(20).map(|r| serde_json::json!({ "path": r.path, "line": r.line, "col": r.col })).collect::<Vec<_>>(),
                });
                changes.push(v);
            }
        }
        let rank = |v: &serde_json::Value| match v["severity"].as_str() {
            Some("high") => 0,
            Some("medium") => 1,
            Some("low") => 2,
            _ => 3,
        };
        changes.sort_by_key(|v| (rank(v), v["path"].as_str().unwrap_or("").to_string()));
        Ok(serde_json::json!({
            "base": cs.base,
            "baseSha": cs.base_sha,
            "target": cs.target,
            "targetSha": cs.target_sha,
            "scanned": files.len(),
            "indexed": indexed,
            "changes": changes,
        }))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "checks task failed"))??;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_scan_parsers() {
        let osv = serde_json::json!({ "results": [{ "source": { "path": "/repo/Cargo.lock", "type": "lockfile" }, "packages": [{
            "package": { "name": "time", "version": "0.1.43", "ecosystem": "crates.io" },
            "vulnerabilities": [{ "id": "RUSTSEC-2020-0071", "summary": "Potential segfault", "database_specific": { "severity": "MODERATE" } }],
            "groups": [{ "ids": ["RUSTSEC-2020-0071", "CVE-2020-26235"], "max_severity": "6.2" }] }] }] });
        let f = parse_osv(&osv, std::path::Path::new("/repo"));
        assert_eq!(f[0]["path"], "Cargo.lock");
        assert_eq!(f[0]["severity"], "medium");
        assert_eq!(f[0]["title"], "time@0.1.43: Potential segfault");

        let npm = serde_json::json!({ "vulnerabilities": { "lodash": { "name": "lodash", "severity": "high", "range": "<4.17.21",
            "via": [{ "source": 1065, "title": "Prototype Pollution", "url": "https://github.com/advisories/GHSA-jf85-cpcp-j695", "severity": "high" }] } } });
        let f = parse_npm_audit(&npm, "web/package-lock.json");
        assert_eq!(f[0]["rule"], "GHSA-jf85-cpcp-j695");
        assert_eq!(f[0]["severity"], "high");

        let sg = serde_json::json!({ "results": [
            { "check_id": "python.lang.security.audit.eval-detected", "path": "app.py", "start": { "line": 3 }, "extra": { "message": "eval", "severity": "ERROR", "lines": "eval(x)" } },
            { "check_id": "python.x", "path": "app.py", "start": { "line": 9 }, "extra": { "severity": "WARNING" } },
        ] });
        let added = [("app.py".to_string(), 3u32)].into_iter().collect();
        let f = parse_semgrep(&sg, &added);
        assert_eq!(f.len(), 1, "line 9 was not added");
        assert_eq!(f[0]["title"], "eval detected");
        assert_eq!(f[0]["severity"], "high");
    }
}

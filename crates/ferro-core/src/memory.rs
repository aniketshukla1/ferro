//! Team review memory (API.md § 17): what reviewers accept and dismiss becomes rules that
//! shape later reviews. Two kinds:
//!
//! - `ignore`: never report a kind of finding (an AI finding type, or a security rule) in
//!   some paths or everywhere, with the reason why.
//! - `convention`: something this team cares about, handed to the AI reviewer as guidance.
//!
//! Team rules live in the repository (`.ferro-rules.json`), so the team shares them through
//! git and reviews changes to them like code. Personal rules, the accept / dismiss signals
//! that suggest new rules, and hit counts stay in ferro's state directory.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const TEAM_FILE: &str = ".ferro-rules.json";
const MAX_SIGNALS: usize = 2000;
const MAX_RULES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub id: String,
    /// `ignore` | `convention`
    pub kind: String,
    /// `ai` | `security` | `any`
    #[serde(rename = "appliesTo")]
    pub applies_to: String,
    /// Security rule id (`secret.aws-key`), or a prefix (`secret.*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// AI finding category (`style`, `performance`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// AI finding title; matched by its words, not its exact wording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Globs the rule is limited to (`tests/**`); empty = everywhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// A convention, in the team's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub author: String,
    #[serde(rename = "createdAt", default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleFile {
    pub version: u32,
    pub rules: Vec<Rule>,
}

/// One accept or dismiss, the raw material for suggestions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Signal {
    /// `accept` | `dismiss`
    pub action: String,
    /// `ai` | `security`
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub title: String,
    pub path: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Personal {
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub signals: Vec<Signal>,
    /// rule id -> (hits, last hit)
    #[serde(default)]
    pub hits: BTreeMap<String, (u64, String)>,
    /// Suggestion keys the user said no to.
    #[serde(default, rename = "dismissedSuggestions")]
    pub dismissed_suggestions: Vec<String>,
}

/// What a rule is matched against.
pub struct Subject<'a> {
    pub source: &'a str,
    pub rule: Option<&'a str>,
    pub category: Option<&'a str>,
    pub title: &'a str,
    pub path: &'a str,
}

pub fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "r_{:x}{:04x}",
        nanos,
        N.fetch_add(1, Ordering::Relaxed) & 0xffff
    )
}

/// Lowercase words of a title (punctuation and code quotes dropped).
fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() > 1)
        .map(str::to_string)
        .collect()
}

/// Same finding type: the rule's words all appear, or the two titles share most words.
pub fn same_title(rule_title: &str, title: &str) -> bool {
    let a = words(rule_title);
    let b = words(title);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.iter().all(|w| b.contains(w)) {
        return true;
    }
    let inter = a.iter().filter(|w| b.contains(w)).count() as f64;
    let union = (a.len() + b.len()) as f64 - inter;
    inter / union >= 0.7
}

fn glob_match(globs: &[String], path: &str) -> bool {
    if globs.is_empty() {
        return true;
    }
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        if let Ok(gl) = globset::GlobBuilder::new(g)
            .literal_separator(false)
            .build()
        {
            b.add(gl);
        }
    }
    b.build().map(|s| s.is_match(path)).unwrap_or(false)
}

/// Does ignore-rule `r` cover this finding?
pub fn matches(r: &Rule, s: &Subject) -> bool {
    if r.kind != "ignore" || (r.applies_to != "any" && r.applies_to != s.source) {
        return false;
    }
    // A rule must say *what* it ignores; a bare path rule would hide everything there.
    if r.rule.is_none() && r.category.is_none() && r.title.is_none() {
        return false;
    }
    if let Some(want) = &r.rule {
        let Some(got) = s.rule else { return false };
        let hit = match want.strip_suffix('*') {
            Some(prefix) => got.starts_with(prefix),
            None => got == want,
        };
        if !hit {
            return false;
        }
    }
    if let Some(c) = &r.category {
        if !s.category.is_some_and(|x| x.eq_ignore_ascii_case(c)) {
            return false;
        }
    }
    if let Some(t) = &r.title {
        if !same_title(t, s.title) {
            return false;
        }
    }
    glob_match(&r.paths, s.path)
}

/// Check a new rule before it is stored. Returns the problem, if any.
pub fn validate(r: &Rule) -> Option<&'static str> {
    match r.kind.as_str() {
        "ignore" => {
            if r.rule.is_none() && r.category.is_none() && r.title.is_none() {
                return Some("an ignore rule needs a rule id, a category or a title");
            }
        }
        "convention" => {
            if r.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
                return Some("a convention needs its text");
            }
        }
        _ => return Some("kind must be ignore or convention"),
    }
    if !["ai", "security", "any"].contains(&r.applies_to.as_str()) {
        return Some("appliesTo must be ai, security or any");
    }
    let too_long = |s: &Option<String>, n: usize| s.as_deref().is_some_and(|x| x.len() > n);
    if too_long(&r.rule, 200)
        || too_long(&r.category, 64)
        || too_long(&r.title, 400)
        || too_long(&r.text, 2000)
        || r.reason.len() > 2000
        || r.paths.len() > 20
        || r.paths
            .iter()
            .any(|p| p.len() > 256 || p.starts_with('/') || p.contains(".."))
    {
        return Some("rule fields are too long, or a path is absolute or climbs out");
    }
    if r.paths.iter().any(|p| globset::Glob::new(p).is_err()) {
        return Some("a path pattern is not a valid glob");
    }
    None
}

// ---------- storage ----------

pub fn parse_team(bytes: &[u8]) -> Vec<Rule> {
    serde_json::from_slice::<RuleFile>(bytes)
        .map(|f| f.rules)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| validate(r).is_none())
        .take(MAX_RULES)
        .collect()
}

pub fn read_team(root: &Path) -> Vec<Rule> {
    std::fs::read(root.join(TEAM_FILE))
        .map(|b| parse_team(&b))
        .unwrap_or_default()
}

/// Write the team file: stable order and formatting, so git diffs show only real changes.
pub fn write_team(root: &Path, rules: &[Rule]) -> Result<(), String> {
    let mut rules = rules.to_vec();
    rules.sort_by(|a, b| {
        (a.created_at.as_str(), a.id.as_str()).cmp(&(b.created_at.as_str(), b.id.as_str()))
    });
    let file = RuleFile { version: 1, rules };
    let mut text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    text.push('\n');
    let path = root.join(TEAM_FILE);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    crate::settings::atomic_write(&path, text.as_bytes())
}

pub fn personal_path(state_dir: &Path) -> PathBuf {
    state_dir.join("memory.json")
}

pub fn read_personal(state_dir: &Path) -> Personal {
    std::fs::read(personal_path(state_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_personal(state_dir: &Path, p: &Personal) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string(p).map_err(|e| e.to_string())?;
    crate::settings::atomic_write(&personal_path(state_dir), text.as_bytes())
}

impl Personal {
    pub fn record(&mut self, s: Signal) {
        self.signals.push(s);
        if self.signals.len() > MAX_SIGNALS {
            let over = self.signals.len() - MAX_SIGNALS;
            self.signals.drain(..over);
        }
    }

    pub fn hit(&mut self, id: &str) {
        let e = self
            .hits
            .entry(id.to_string())
            .or_insert((0, String::new()));
        e.0 += 1;
        e.1 = now_iso();
    }
}

// ---------- suggestions ----------

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Suggestion {
    /// Stable key, so "not now" sticks.
    pub key: String,
    /// The rule to create if the user agrees (id empty until then).
    pub rule: Rule,
    pub count: usize,
    pub examples: Vec<String>,
    /// Plain words: why ferro suggests it.
    pub why: String,
}

/// The shortest glob covering `paths`: `dir/**` when they share a directory, else none.
fn common_glob(paths: &[&str]) -> Vec<String> {
    let dirs: Vec<Vec<&str>> = paths
        .iter()
        .map(|p| p.split('/').collect::<Vec<_>>())
        .map(|mut v| {
            v.pop();
            v
        })
        .collect();
    let Some(first) = dirs.first() else {
        return vec![];
    };
    let mut n = first.len();
    for d in &dirs[1..] {
        n = n.min(first.iter().zip(d).take_while(|(a, b)| a == b).count());
    }
    if n == 0 {
        vec![]
    } else {
        vec![format!("{}/**", first[..n].join("/"))]
    }
}

/// Rules worth proposing: the same finding type dismissed twice or more (→ ignore it there),
/// accepted twice or more (→ a convention the AI should keep checking). Nothing already
/// covered by a rule, nothing the user turned down.
pub fn suggestions(p: &Personal, team: &[Rule]) -> Vec<Suggestion> {
    let all: Vec<&Rule> = team.iter().chain(p.rules.iter()).collect();
    let mut groups: BTreeMap<String, Vec<&Signal>> = BTreeMap::new();
    for s in &p.signals {
        let what = s.rule.clone().unwrap_or_else(|| words(&s.title).join(" "));
        let key = format!(
            "{}|{}|{}|{}",
            s.action,
            s.source,
            s.category.as_deref().unwrap_or(""),
            what
        );
        groups.entry(key).or_default().push(s);
    }
    let mut out = vec![];
    for (key, sigs) in groups {
        if sigs.len() < 2 || p.dismissed_suggestions.contains(&key) {
            continue;
        }
        let s0 = sigs[0];
        let paths: Vec<&str> = sigs.iter().map(|s| s.path.as_str()).collect();
        let mut examples: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        examples.dedup();
        examples.truncate(5);
        if s0.action == "dismiss" {
            let covered = sigs.iter().all(|s| {
                all.iter().any(|r| {
                    matches(
                        r,
                        &Subject {
                            source: &s.source,
                            rule: s.rule.as_deref(),
                            category: s.category.as_deref(),
                            title: &s.title,
                            path: &s.path,
                        },
                    )
                })
            });
            if covered {
                continue;
            }
            let rule = Rule {
                id: String::new(),
                kind: "ignore".into(),
                applies_to: s0.source.clone(),
                rule: s0.rule.clone(),
                category: if s0.rule.is_some() {
                    None
                } else {
                    s0.category.clone()
                },
                title: if s0.rule.is_some() {
                    None
                } else {
                    Some(s0.title.clone())
                },
                paths: common_glob(&paths),
                text: None,
                reason: String::new(),
                author: String::new(),
                created_at: String::new(),
            };
            let where_ = if rule.paths.is_empty() {
                "in different places".to_string()
            } else {
                format!("in {}", rule.paths[0])
            };
            out.push(Suggestion {
                why: format!("Dismissed {} times {where_}", sigs.len()),
                key,
                rule,
                count: sigs.len(),
                examples,
            });
        } else {
            let exists = all.iter().any(|r| {
                r.kind == "convention"
                    && r.text.as_deref().is_some_and(|t| same_title(&s0.title, t))
            });
            if exists {
                continue;
            }
            let rule = Rule {
                id: String::new(),
                kind: "convention".into(),
                applies_to: "ai".into(),
                rule: None,
                category: s0.category.clone(),
                title: None,
                paths: vec![],
                text: Some(format!("Keep flagging: {}", s0.title)),
                reason: String::new(),
                author: String::new(),
                created_at: String::new(),
            };
            out.push(Suggestion {
                why: format!("Accepted {} times", sigs.len()),
                key,
                rule,
                count: sigs.len(),
                examples,
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.count));
    out.truncate(20);
    out
}

/// Guidance for the AI reviewer: the team's conventions, and what not to report.
pub fn review_guidance(rules: &[Rule]) -> String {
    let mut conv = vec![];
    let mut ignore = vec![];
    for r in rules {
        let scope = if r.paths.is_empty() {
            String::new()
        } else {
            format!(" (in {})", r.paths.join(", "))
        };
        match r.kind.as_str() {
            "convention" if r.applies_to != "security" => {
                if let Some(t) = &r.text {
                    conv.push(format!("- {}{scope}", t.trim()));
                }
            }
            "ignore" if r.applies_to != "security" => {
                let what = match (&r.title, &r.category) {
                    (Some(t), Some(c)) => format!("{c}: {t}"),
                    (Some(t), None) => t.clone(),
                    (None, Some(c)) => format!("any {c} finding"),
                    _ => continue,
                };
                let why = if r.reason.trim().is_empty() {
                    String::new()
                } else {
                    format!(" — {}", r.reason.trim())
                };
                ignore.push(format!("- {what}{scope}{why}"));
            }
            _ => {}
        }
    }
    let mut out = String::new();
    if !conv.is_empty() {
        out.push_str("Team conventions (this team's reviewers care about these):\n");
        out.push_str(&conv.join("\n"));
        out.push('\n');
    }
    if !ignore.is_empty() {
        out.push_str("Do not report these; the team has decided they are not issues here:\n");
        out.push_str(&ignore.join("\n"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignore(
        rule: Option<&str>,
        category: Option<&str>,
        title: Option<&str>,
        paths: &[&str],
    ) -> Rule {
        Rule {
            id: new_id(),
            kind: "ignore".into(),
            applies_to: if rule.is_some() {
                "security".into()
            } else {
                "ai".into()
            },
            rule: rule.map(str::to_string),
            category: category.map(str::to_string),
            title: title.map(str::to_string),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            text: None,
            reason: "x".into(),
            author: String::new(),
            created_at: now_iso(),
        }
    }

    fn sub<'a>(
        source: &'a str,
        rule: Option<&'a str>,
        category: Option<&'a str>,
        title: &'a str,
        path: &'a str,
    ) -> Subject<'a> {
        Subject {
            source,
            rule,
            category,
            title,
            path,
        }
    }

    #[test]
    fn matching_by_rule_title_category_and_path() {
        let r = ignore(
            Some("secret.*"),
            None,
            None,
            &["tests/**", "**/fixtures/**"],
        );
        assert!(matches(
            &r,
            &sub(
                "security",
                Some("secret.aws-key"),
                None,
                "AWS access key",
                "tests/it.rs"
            )
        ));
        assert!(matches(
            &r,
            &sub(
                "security",
                Some("secret.aws-key"),
                None,
                "",
                "web/fixtures/a.json"
            )
        ));
        assert!(
            !matches(
                &r,
                &sub("security", Some("secret.aws-key"), None, "", "src/main.rs")
            ),
            "outside the paths"
        );
        assert!(
            !matches(
                &r,
                &sub("security", Some("js.eval"), None, "", "tests/a.js")
            ),
            "another rule"
        );
        assert!(
            !matches(
                &r,
                &sub("ai", Some("secret.aws-key"), None, "", "tests/a.rs")
            ),
            "another source"
        );

        let t = ignore(
            None,
            Some("style"),
            Some("Prefer `is_empty()` over `len() == 0`"),
            &[],
        );
        assert!(matches(
            &t,
            &sub(
                "ai",
                None,
                Some("style"),
                "Prefer is_empty() over len() == 0",
                "src/x.rs"
            )
        ));
        assert!(
            matches(
                &t,
                &sub(
                    "ai",
                    None,
                    Some("Style"),
                    "Use is_empty() instead of len() == 0",
                    "src/y.rs"
                )
            ) == same_title(
                "Prefer `is_empty()` over `len() == 0`",
                "Use is_empty() instead of len() == 0"
            )
        );
        assert!(
            !matches(
                &t,
                &sub(
                    "ai",
                    None,
                    Some("bug"),
                    "Prefer is_empty() over len() == 0",
                    "src/x.rs"
                )
            ),
            "category differs"
        );
        // No "what": never matches, never validates.
        let bare = ignore(None, None, None, &["src/**"]);
        assert!(!matches(
            &bare,
            &sub("ai", None, Some("bug"), "anything", "src/a.rs")
        ));
        assert!(validate(&bare).is_some());
        assert!(validate(&ignore(Some("x"), None, None, &["../etc/**"])).is_some());
    }

    #[test]
    fn team_file_roundtrip_is_stable() {
        let d = tempfile::tempdir().unwrap();
        let a = ignore(Some("secret.*"), None, None, &["tests/**"]);
        let mut b = ignore(None, Some("style"), Some("t"), &[]);
        b.created_at = "2000-01-01T00:00:00Z".into();
        write_team(d.path(), &[a.clone(), b.clone()]).unwrap();
        let back = read_team(d.path());
        assert_eq!(back, vec![b.clone(), a.clone()], "oldest first");
        let first = std::fs::read_to_string(d.path().join(TEAM_FILE)).unwrap();
        write_team(d.path(), &back).unwrap();
        assert_eq!(
            first,
            std::fs::read_to_string(d.path().join(TEAM_FILE)).unwrap()
        );
        assert!(first.ends_with("}\n"));
        // Invalid rules in a hand-edited file are skipped, not fatal.
        std::fs::write(d.path().join(TEAM_FILE), r#"{"version":1,"rules":[{"id":"r1","kind":"ignore","appliesTo":"ai","paths":["src/**"]}]}"#).unwrap();
        assert!(read_team(d.path()).is_empty());
    }

    #[test]
    fn suggestions_from_repeated_signals() {
        let mut p = Personal::default();
        let sig = |action: &str, title: &str, path: &str| Signal {
            action: action.into(),
            source: "ai".into(),
            rule: None,
            category: Some("style".into()),
            title: title.into(),
            path: path.into(),
            at: now_iso(),
        };
        p.record(sig(
            "dismiss",
            "Prefer is_empty() over len() == 0",
            "tests/a.rs",
        ));
        p.record(sig(
            "dismiss",
            "Prefer is_empty() over len() == 0",
            "tests/unit/b.rs",
        ));
        p.record(sig("dismiss", "Magic number", "src/x.rs"));
        p.record(sig("accept", "Unchecked unwrap on user input", "src/a.rs"));
        p.record(sig("accept", "Unchecked unwrap on user input", "src/b.rs"));
        let s = suggestions(&p, &[]);
        assert_eq!(s.len(), 2, "{s:#?}");
        let ign = s.iter().find(|x| x.rule.kind == "ignore").unwrap();
        assert_eq!(ign.rule.paths, vec!["tests/**"]);
        assert_eq!(ign.count, 2);
        let conv = s.iter().find(|x| x.rule.kind == "convention").unwrap();
        assert_eq!(
            conv.rule.text.as_deref(),
            Some("Keep flagging: Unchecked unwrap on user input")
        );
        // Once a rule covers it, or the user says no, the suggestion goes away.
        let mut covered = ign.rule.clone();
        covered.id = new_id();
        assert_eq!(suggestions(&p, &[covered]).len(), 1);
        p.dismissed_suggestions.push(conv.key.clone());
        assert_eq!(suggestions(&p, &[]).len(), 1);
    }

    #[test]
    fn guidance_lists_conventions_and_ignores() {
        let mut c = ignore(None, None, None, &[]);
        c.kind = "convention".into();
        c.text = Some("Every public function has a doc comment".into());
        let i = ignore(None, Some("style"), Some("Magic number"), &["tests/**"]);
        let sec = ignore(Some("secret.*"), None, None, &[]);
        let g = review_guidance(&[c, i, sec]);
        assert!(g.contains("- Every public function has a doc comment"));
        assert!(g.contains("- style: Magic number (in tests/**) — x"));
        assert!(
            !g.contains("secret"),
            "security rules do not reach the AI prompt"
        );
    }
}

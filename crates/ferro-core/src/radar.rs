//! Breaking-change radar (API.md § 16.1): the definitions a change removes, renames or
//! re-signatures, from the tree-sitter outline of both sides of a file. Callers that still use
//! them come from the reference index (the server joins the two).

use crate::symbols::{outline_ts, Symbol};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApiChange {
    pub name: String,
    /// `Parent::name` for nested definitions (methods).
    pub qualified: String,
    pub kind: &'static str,
    /// `removed` | `renamed` | `signature`
    pub change: &'static str,
    /// Visible outside its module by the language's rules (`pub`, `export`, capitalized Go, …).
    pub public: bool,
    #[serde(rename = "oldLine")]
    pub old_line: usize,
    #[serde(rename = "oldSignature")]
    pub old_detail: String,
    #[serde(rename = "newName", skip_serializing_if = "Option::is_none")]
    pub new_name: Option<String>,
    #[serde(rename = "newLine", skip_serializing_if = "Option::is_none")]
    pub new_line: Option<usize>,
    #[serde(rename = "newSignature", skip_serializing_if = "Option::is_none")]
    pub new_detail: Option<String>,
}

const KINDS: [&str; 10] = [
    "function",
    "method",
    "class",
    "struct",
    "enum",
    "interface",
    "trait",
    "type",
    "const",
    "macro",
];
const CALLABLE: [&str; 3] = ["function", "method", "macro"];

/// Test code: removing a test never breaks callers.
pub fn is_test_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    p.starts_with("tests/")
        || p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.starts_with("test/")
        || p.contains("/test/")
        || p.contains("/mock/")
        || p.contains("/mocks/")
        || p.contains("/__mocks__/")
        || p.contains("/fixtures/")
        || p.contains("/testdata/")
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
}

fn ext_of(path: &str) -> &str {
    path.rsplit_once('.').map(|(_, e)| e).unwrap_or("")
}

fn is_public(ext: &str, name: &str, detail: &str) -> bool {
    let d = detail.trim_start();
    match ext {
        "rs" => d.starts_with("pub ") || d.starts_with("pub("),
        "go" => name.chars().next().is_some_and(char::is_uppercase),
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => {
            d.starts_with("export ") || d.starts_with("public ")
        }
        "py" | "rb" => !name.starts_with('_'),
        "java" | "cs" | "php" => d.contains("public "),
        _ => true,
    }
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Outline with qualified names (`Parent::name`): definitions only, never ones nested inside a
/// function (closures, local helpers, `x.onclick = …` handlers), tests left out, and names that
/// appear twice under one parent (overloads, duplicate handlers) dropped as ambiguous.
fn defs(ext: &str, text: &str) -> Vec<(String, Symbol)> {
    let Some(mut syms) = outline_ts(ext, text) else {
        return vec![];
    };
    syms.sort_by_key(|s| (s.line, s.depth));
    let mut stack: Vec<(usize, String, &'static str)> = vec![];
    let mut out: Vec<(String, Symbol)> = vec![];
    for s in syms {
        while stack.last().is_some_and(|(d, _, _)| *d >= s.depth) {
            stack.pop();
        }
        let nested_in_fn = stack.iter().any(|(_, _, k)| CALLABLE.contains(k));
        let mut q: Vec<String> = stack.iter().map(|(_, n, _)| n.clone()).collect();
        q.push(s.name.clone());
        let qualified = q.join("::");
        stack.push((s.depth, s.name.clone(), s.kind));
        let in_tests = q.iter().any(|n| n == "tests" || n == "test")
            || s.name.starts_with("test_")
            || s.detail.contains("#[test]");
        if in_tests || nested_in_fn || !KINDS.contains(&s.kind) {
            continue;
        }
        out.push((qualified, s));
    }
    let mut seen: HashMap<(String, &'static str), usize> = HashMap::new();
    for (q, s) in &out {
        *seen.entry((q.clone(), s.kind)).or_default() += 1;
    }
    out.retain(|(q, s)| seen[&(q.clone(), s.kind)] == 1);
    out
}

/// What `path` loses between `old` and `new` (`None` = the file does not exist on that side).
pub fn api_changes(path: &str, old: Option<&str>, new: Option<&str>) -> Vec<ApiChange> {
    if is_test_path(path) {
        return vec![];
    }
    let ext = ext_of(path);
    let before = old.map(|t| defs(ext, t)).unwrap_or_default();
    let after = new.map(|t| defs(ext, t)).unwrap_or_default();
    let key = |q: &str, s: &Symbol| format!("{}\u{1f}{q}", s.kind);
    let after_by: HashMap<String, &Symbol> = after.iter().map(|(q, s)| (key(q, s), s)).collect();
    let before_keys: std::collections::HashSet<String> =
        before.iter().map(|(q, s)| key(q, s)).collect();
    // New definitions that did not exist before: rename candidates.
    let mut fresh: Vec<(&String, &Symbol)> = after
        .iter()
        .filter(|(q, s)| !before_keys.contains(&key(q, s)))
        .map(|(q, s)| (q, s))
        .collect();
    let mut out = vec![];
    for (q, s) in &before {
        let public = is_public(ext, &s.name, &s.detail);
        match after_by.get(&key(q, s)) {
            Some(n) => {
                if CALLABLE.contains(&s.kind) && norm(&s.detail) != norm(&n.detail) {
                    out.push(ApiChange {
                        name: s.name.clone(),
                        qualified: q.clone(),
                        kind: s.kind,
                        change: "signature",
                        public,
                        old_line: s.line,
                        old_detail: s.detail.clone(),
                        new_name: None,
                        new_line: Some(n.line),
                        new_detail: Some(n.detail.clone()),
                    });
                }
            }
            None => {
                // Renamed: same kind and parent, same signature once the name is swapped.
                let parent = q.rsplit_once("::").map(|(p, _)| p).unwrap_or("");
                let want = norm(&s.detail.replace(&s.name, "\u{1}"));
                let hit = fresh.iter().position(|(nq, n)| {
                    n.kind == s.kind
                        && nq.rsplit_once("::").map(|(p, _)| p).unwrap_or("") == parent
                        && norm(&n.detail.replace(&n.name, "\u{1}")) == want
                });
                let renamed = hit.map(|i| fresh.remove(i).1);
                out.push(ApiChange {
                    name: s.name.clone(),
                    qualified: q.clone(),
                    kind: s.kind,
                    change: if renamed.is_some() {
                        "renamed"
                    } else {
                        "removed"
                    },
                    public,
                    old_line: s.line,
                    old_detail: s.detail.clone(),
                    new_name: renamed.map(|n| n.name.clone()),
                    new_line: renamed.map(|n| n.line),
                    new_detail: renamed.map(|n| n.detail.clone()),
                });
            }
        }
    }
    out
}

#[cfg(all(test, feature = "ts-rust"))]
mod tests {
    use super::*;

    const OLD: &str = "pub fn keep(a: u32) -> u32 { a }\n\npub fn gone() {}\n\nfn helper(x: i32) {}\n\npub struct Store;\n\nimpl Store {\n    pub fn open(path: &str) -> Store { Store }\n    pub fn len(&self) -> usize { 0 }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it_works() {}\n}\n";
    const NEW: &str = "pub fn keep(a: u32, b: u32) -> u32 { a }\n\nfn assist(x: i32) {}\n\npub struct Store;\n\nimpl Store {\n    pub fn open(path: &str) -> Store { Store }\n}\n";

    #[test]
    fn removed_renamed_and_signature_changes() {
        let c = api_changes("src/lib.rs", Some(OLD), Some(NEW));
        let find = |n: &str| c.iter().find(|x| x.name == n).cloned();
        let keep = find("keep").unwrap();
        assert_eq!(keep.change, "signature");
        assert!(keep.new_detail.unwrap().contains("b: u32"));
        assert_eq!(find("gone").unwrap().change, "removed");
        assert!(find("gone").unwrap().public);
        let helper = find("helper").unwrap();
        assert_eq!(helper.change, "renamed");
        assert_eq!(helper.new_name.as_deref(), Some("assist"));
        assert!(!helper.public);
        let len = find("len").unwrap();
        assert_eq!(len.qualified, "Store::len");
        assert_eq!(len.change, "removed");
        assert!(find("open").is_none(), "unchanged");
        assert!(find("it_works").is_none(), "tests are not API");
    }

    #[test]
    fn deleted_file_and_test_files() {
        let c = api_changes("src/lib.rs", Some(OLD), None);
        assert!(c.iter().all(|x| x.change == "removed"));
        assert!(c.iter().any(|x| x.name == "Store"));
        assert!(api_changes("tests/it.rs", Some(OLD), None).is_empty());
        assert!(is_test_path("web/src/a.spec.js") && is_test_path("pkg/x_test.go"));
        assert!(!is_test_path("src/testing_utils.rs"));
    }
}

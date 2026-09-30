//! Regex symbol extractor (B1 source; B6 swaps in tree-sitter for supported
//! languages). Ported from the legacy frontend patterns, extended per API.md
//! § 4.7. Used by both the `/file/outline` endpoint and read-only agent tools.

struct Rule {
    re: regex::Regex,
    kind: &'static str,
    name_group: usize,
}

fn rules_for(ext: &str) -> Vec<Rule> {
    let r = |pat: &str, kind: &'static str, g: usize| Rule {
        re: regex::Regex::new(pat).unwrap(),
        kind,
        name_group: g,
    };
    match ext {
        "rs" => vec![
            r(
                r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)",
                "function",
                1,
            ),
            r(r"^\s*(?:pub\s+)?struct\s+([A-Za-z_]\w*)", "struct", 1),
            r(r"^\s*(?:pub\s+)?enum\s+([A-Za-z_]\w*)", "enum", 1),
            r(r"^\s*(?:pub\s+)?trait\s+([A-Za-z_]\w*)", "trait", 1),
            r(r"^\s*(?:pub\s+)?mod\s+([A-Za-z_]\w*)", "module", 1),
            r(
                r"^\s*impl(?:\s+[A-Za-z_][\w:]*)?\s+([A-Za-z_][\w:]*)",
                "impl",
                1,
            ),
            r(r"^\s*macro_rules!\s*([A-Za-z_]\w*)", "macro", 1),
        ],
        "go" => vec![
            r(r"^\s*func\s+(?:\([^)]*\)\s+)?([A-Za-z_]\w*)", "function", 1),
            r(r"^\s*type\s+([A-Za-z_]\w*)", "type", 1),
        ],
        "py" => vec![
            r(r"^\s*(?:async\s+def|def)\s+([A-Za-z_]\w*)", "function", 1),
            r(r"^\s*class\s+([A-Za-z_]\w*)", "class", 1),
        ],
        "js" | "jsx" | "mjs" | "cjs" => vec![
            r(
                r"^\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_]\w*)",
                "function",
                1,
            ),
            r(r"^\s*(?:export\s+)?class\s+([A-Za-z_]\w*)", "class", 1),
            r(
                r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_]\w*)\s*=",
                "var",
                1,
            ),
        ],
        "ts" | "tsx" | "mts" | "cts" => vec![
            r(
                r"^\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_]\w*)",
                "function",
                1,
            ),
            r(r"^\s*(?:export\s+)?class\s+([A-Za-z_]\w*)", "class", 1),
            r(
                r"^\s*(?:export\s+)?interface\s+([A-Za-z_]\w*)",
                "interface",
                1,
            ),
            r(r"^\s*(?:export\s+)?type\s+([A-Za-z_]\w*)", "type", 1),
            r(
                r"^\s*(?:export\s+)?const\s+([A-Za-z_]\w*)\s*[:=]",
                "const",
                1,
            ),
        ],
        "java" | "kt" | "kts" => vec![
            r(
                r"^\s*(?:public|private|protected)?\s*(?:static\s+)?(?:[\w<>\[\]]+\s+)+([A-Za-z_]\w*)\s*\(",
                "method",
                1,
            ),
            r(
                r"^\s*(?:public|private|protected)?\s*(?:abstract\s+|final\s+)?(?:class|interface|enum|record)\s+([A-Za-z_]\w*)",
                "class",
                1,
            ),
        ],
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" => vec![
            r(
                r"^[\w][\w\s\*&:<>]*\s+([A-Za-z_]\w*)\s*\([^;]*\)\s*\{?",
                "function",
                1,
            ),
            r(r"^\s*(?:class|struct|enum)\s+([A-Za-z_]\w*)", "class", 1),
        ],
        "cs" => vec![
            r(
                r"^\s*(?:public|private|protected|internal)?\s*(?:static\s+|virtual\s+|override\s+|async\s+)*(?:[\w<>\[\]]+\s+)+([A-Za-z_]\w*)\s*\(",
                "method",
                1,
            ),
            r(
                r"^\s*(?:public|private|protected|internal)?\s*(?:class|struct|interface|enum|record)\s+([A-Za-z_]\w*)",
                "class",
                1,
            ),
            r(r"^\s*namespace\s+([A-Za-z_.]\w*)", "module", 1),
        ],
        "rb" => vec![
            r(r"^\s*def\s+(?:self\.)?([A-Za-z_]\w*[?!]?)", "method", 1),
            r(r"^\s*(?:class|module)\s+([A-Za-z_:]\w*)", "class", 1),
        ],
        "php" => vec![
            r(
                r"^\s*(?:public|private|protected)?\s*(?:static\s+)?function\s+([A-Za-z_]\w*)",
                "function",
                1,
            ),
            r(
                r"^\s*(?:abstract\s+|final\s+)?(?:class|interface|trait|enum)\s+([A-Za-z_]\w*)",
                "class",
                1,
            ),
        ],
        "swift" => vec![
            r(
                r"^\s*(?:public|private|internal|fileprivate|open)?\s*(?:static\s+|class\s+)?func\s+([A-Za-z_]\w*)",
                "function",
                1,
            ),
            r(
                r"^\s*(?:public|private|internal|fileprivate|open)?\s*(?:class|struct|enum|protocol|extension)\s+([A-Za-z_]\w*)",
                "class",
                1,
            ),
        ],
        "sh" | "bash" => vec![r(
            r"^\s*(?:function\s+)?([A-Za-z_]\w*)\s*\(\s*\)",
            "function",
            1,
        )],
        "md" | "markdown" => vec![r(r"^(#{1,3})\s+(.+)$", "heading", 2)],
        _ => vec![],
    }
}

/// Extract symbols from text. Returns (name, kind, line, depth).
/// Depth comes from indentation nesting.
pub fn extract(ext: &str, text: &str) -> Vec<(String, String, usize, usize)> {
    let rules = rules_for(ext);
    let mut out = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for (i, line) in text.split('\n').enumerate().take(200_000) {
        let indent = line.len() - line.trim_start().len();
        for r in &rules {
            if let Some(m) = r.re.captures(line) {
                if let Some(name) = m.get(r.name_group) {
                    let name = name.as_str().trim().to_string();
                    if name.is_empty() {
                        break;
                    }
                    while stack.last().map(|&ind| ind >= indent).unwrap_or(false) {
                        stack.pop();
                    }
                    let depth = stack.len();
                    stack.push(indent);
                    out.push((name, r.kind.to_string(), i + 1, depth));
                    if out.len() >= 5000 {
                        return out;
                    }
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_shapes() {
        let syms = extract("rs", "pub struct Foo;\n\nimpl Foo {\n    pub async fn bar(&self) {}\n}\nmacro_rules! baz {}\n");
        let names: Vec<&str> = syms.iter().map(|s| s.0.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"bar"));
        assert!(syms.iter().find(|s| s.0 == "bar").unwrap().1 == "function");
    }

    #[test]
    fn depth_nests_by_indent() {
        let syms = extract("py", "class A:\n    def f(self):\n        pass\ndef g():\n");
        assert_eq!(syms[0].3, 0);
        assert_eq!(syms[1].3, 1);
        assert_eq!(syms[2].3, 0);
    }

    #[test]
    fn new_languages() {
        assert!(!extract(
            "java",
            "public class Foo {\n public static void main(String[] a) {}\n}"
        )
        .is_empty());
        assert!(!extract("c", "int main(void) {\n}\n").is_empty());
        assert!(!extract("rb", "class Foo\n def bar?\n end\nend\n").is_empty());
        assert!(!extract("swift", "public func f() {}\n").is_empty());
        assert!(!extract("md", "# Title\n").is_empty());
        assert!(extract("xyz", "nothing here\n").is_empty());
    }
}

//! Built-in security scan of a change (API.md § 16.3): secrets and risky patterns on the lines a
//! change adds. Offline, no external tools, nothing leaves the machine. Rules are deliberately
//! narrow (few false positives); the deep scan adds osv-scanner, cargo-audit, npm audit and
//! semgrep when installed.

use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SecFinding {
    pub rule: &'static str,
    /// `secret` | `code` | `config` | `dependency`
    pub category: &'static str,
    /// `critical` | `high` | `medium` | `low`
    pub severity: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    pub path: String,
    pub line: u32,
    /// The line, trimmed and with secrets masked.
    pub excerpt: String,
}

struct Rule {
    id: &'static str,
    category: &'static str,
    severity: &'static str,
    title: &'static str,
    detail: &'static str,
    /// File extensions (without the dot) the rule applies to; empty = every text file.
    exts: &'static [&'static str],
    re: &'static str,
}

const JS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "cjs", "vue", "svelte"];
const PY: &[&str] = &["py"];
const GO: &[&str] = &["go"];
const RS: &[&str] = &["rs"];
const SQL_HOSTS: &[&str] = &[
    "js", "jsx", "ts", "tsx", "mjs", "cjs", "py", "go", "rs", "java", "rb", "php", "cs", "kt",
];
const SHELLISH: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "yml",
    "yaml",
    "md",
    "dockerfile",
    "txt",
    "ps1",
];

const RULES: &[Rule] = &[
    // ---- secrets ----
    Rule { id: "secret.private-key", category: "secret", severity: "critical", title: "Private key committed", detail: "Anyone with the repository can use this key. Remove it, rotate it, and load it from a secret store.", exts: &[], re: r"-----BEGIN [A-Z ]*PRIVATE KEY-----" },
    Rule { id: "secret.aws-key", category: "secret", severity: "critical", title: "AWS access key", detail: "Rotate the key in IAM and read it from the environment or a secret manager.", exts: &[], re: r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b" },
    Rule { id: "secret.github-token", category: "secret", severity: "critical", title: "GitHub token", detail: "Revoke it at github.com/settings/tokens and read it from the environment.", exts: &[], re: r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{50,})" },
    Rule { id: "secret.stripe-live", category: "secret", severity: "critical", title: "Stripe live secret key", detail: "Roll the key in the Stripe dashboard.", exts: &[], re: r"\b[rs]k_live_[0-9A-Za-z]{20,}" },
    Rule { id: "secret.slack-token", category: "secret", severity: "high", title: "Slack token", detail: "Revoke the token and read it from the environment.", exts: &[], re: r"\bxox[abprs]-[A-Za-z0-9-]{10,}" },
    Rule { id: "secret.ai-key", category: "secret", severity: "high", title: "AI provider API key", detail: "Revoke the key with the provider and read it from the environment.", exts: &[], re: r"\bsk-(?:ant-)?[A-Za-z0-9_-]{20,}" },
    Rule { id: "secret.google-key", category: "secret", severity: "high", title: "Google API key", detail: "Restrict or rotate the key in the Google Cloud console.", exts: &[], re: r"\bAIza[0-9A-Za-z_-]{35}\b" },
    Rule { id: "secret.assignment", category: "secret", severity: "high", title: "Hard-coded secret", detail: "A password, token or key is written into the code. Read it from the environment or a secret store.", exts: &[], re: r#"(?i)\b(?:api[_-]?key|secret(?:[_-]?key)?|access[_-]?token|auth[_-]?token|passwd|password)\b["']?\s*[:=]\s*["'][^"'\s]{12,}["']"# },
    // ---- code ----
    Rule { id: "js.eval", category: "code", severity: "high", title: "eval / new Function", detail: "Running strings as code allows injection. Parse data instead (JSON.parse) or use a lookup table.", exts: JS, re: r"\beval\s*\(|\bnew\s+Function\s*\(" },
    Rule { id: "js.html-sink", category: "code", severity: "medium", title: "HTML injection sink", detail: "Setting HTML from strings is an XSS risk. Use textContent or build nodes; sanitize if HTML is required.", exts: JS, re: r"\.(?:innerHTML|outerHTML)\s*[+]?=|\binsertAdjacentHTML\s*\(|\bdocument\.write\s*\(|\bdangerouslySetInnerHTML\b" },
    Rule { id: "js.tls-off", category: "config", severity: "high", title: "TLS verification disabled", detail: "Connections accept any certificate, so traffic can be intercepted.", exts: JS, re: r#"rejectUnauthorized\s*:\s*false|NODE_TLS_REJECT_UNAUTHORIZED\s*=\s*["']?0"# },
    Rule { id: "js.shell-exec", category: "code", severity: "medium", title: "Shell command from a string", detail: "exec/execSync run through a shell; with user input this is command injection. Prefer execFile/spawn with an argument array.", exts: JS, re: r"\b(?:child_process\.)?exec(?:Sync)?\s*\(\s*(?:`[^`]*\$\{|[^,)]*\+)" },
    Rule { id: "py.eval", category: "code", severity: "high", title: "eval / exec", detail: "Running strings as code allows injection. Use ast.literal_eval or explicit parsing.", exts: PY, re: r"(?:^|[^.\w])(?:eval|exec)\s*\(" },
    Rule { id: "py.shell-true", category: "code", severity: "high", title: "subprocess with shell=True", detail: "The command goes through a shell; with user input this is command injection. Pass an argument list without shell=True.", exts: PY, re: r"subprocess\.\w+\(.*shell\s*=\s*True|\bos\.system\s*\(" },
    Rule { id: "py.pickle", category: "code", severity: "high", title: "pickle / marshal load", detail: "Loading pickles from untrusted data runs arbitrary code. Use JSON or a safe format.", exts: PY, re: r"\b(?:pickle|cPickle|marshal|dill)\.loads?\s*\(" },
    Rule { id: "py.yaml-load", category: "code", severity: "medium", title: "yaml.load without SafeLoader", detail: "yaml.load can construct arbitrary objects. Use yaml.safe_load.", exts: PY, re: r"\byaml\.load\s*\((?:[^)]*)$|\byaml\.load\s*\([^)]*\)" },
    Rule { id: "py.tls-off", category: "config", severity: "high", title: "TLS verification disabled", detail: "verify=False accepts any certificate, so traffic can be intercepted.", exts: PY, re: r"\bverify\s*=\s*False\b" },
    Rule { id: "go.tls-off", category: "config", severity: "high", title: "TLS verification disabled", detail: "InsecureSkipVerify accepts any certificate, so traffic can be intercepted.", exts: GO, re: r"InsecureSkipVerify\s*:\s*true" },
    Rule { id: "go.shell", category: "code", severity: "medium", title: "Shell command via sh -c", detail: "With user input this is command injection. Call the program directly with arguments.", exts: GO, re: r#"exec\.Command(?:Context)?\([^)]*"(?:sh|bash)"\s*,\s*"-c""# },
    Rule { id: "rs.tls-off", category: "config", severity: "high", title: "TLS verification disabled", detail: "The client accepts any certificate, so traffic can be intercepted.", exts: RS, re: r"danger_accept_invalid_(?:certs|hostnames)\s*\(\s*true\s*\)" },
    Rule { id: "rs.shell", category: "code", severity: "medium", title: "Shell command via sh -c", detail: "With user input this is command injection. Call the program directly with arguments.", exts: RS, re: r#"Command::new\(\s*"(?:sh|bash)"\s*\)[^;]*"-c""# },
    Rule { id: "sql.concat", category: "code", severity: "medium", title: "SQL built from strings", detail: "Concatenating or formatting values into SQL allows injection. Use query parameters.", exts: SQL_HOSTS, re: r#"(?i)["'`]\s*(?:SELECT\s[^"'`]*\sFROM|INSERT\s+INTO|UPDATE\s+\w+\s+SET|DELETE\s+FROM)\b[^"'`]*(?:["'`]\s*\+|\$\{|%s|\{\})"# },
    Rule { id: "weak-hash", category: "code", severity: "low", title: "MD5 / SHA-1", detail: "Broken for passwords and signatures. Use SHA-256+ (or argon2/bcrypt for passwords).", exts: SQL_HOSTS, re: r"\b(?:hashlib\.(?:md5|sha1)|md5\.New|sha1\.New|createHash\(\s*['\x22](?:md5|sha1)['\x22])\s*\(" },
    Rule { id: "pipe-to-shell", category: "config", severity: "medium", title: "Download piped into a shell", detail: "Runs whatever the server returns. Download, verify a checksum, then run.", exts: SHELLISH, re: r"\b(?:curl|wget)\b[^|\n]*\|\s*(?:sudo\s+)?(?:sh|bash|zsh)\b" },
    Rule { id: "world-writable", category: "config", severity: "medium", title: "World-writable permissions", detail: "Any user on the machine can modify this file or directory.", exts: &[], re: r"\bchmod\s+(?:-R\s+)?0?777\b|(?:chmod|Chmod|set_mode|from_mode|mkdir|makedirs|MkdirAll)\s*\([^)]*\b0o?777\b" },
    Rule { id: "cors-any", category: "config", severity: "medium", title: "CORS allows any origin", detail: "Any website can call this endpoint with the user's browser.", exts: &[], re: r#"Access-Control-Allow-Origin["']?\s*[:,=]\s*["']\*["']"# },
    Rule { id: "gha.pr-target", category: "config", severity: "high", title: "pull_request_target workflow", detail: "Runs with repository secrets on code from forks. Never check out or run the PR's code in it.", exts: &["yml", "yaml"], re: r"^\s*pull_request_target\s*:?" },
    Rule { id: "gha.injection", category: "config", severity: "medium", title: "Untrusted input in a workflow command", detail: "Event fields like titles and branch names can contain shell. Pass them through env instead.", exts: &["yml", "yaml"], re: r"\$\{\{\s*github\.(?:event\.(?:issue|pull_request|comment|review|head_commit)\.[\w.]*(?:title|body|message|name|ref|label)|head_ref)\s*\}\}" },
];

fn compiled() -> &'static Vec<(Regex, &'static Rule)> {
    static C: OnceLock<Vec<(Regex, &'static Rule)>> = OnceLock::new();
    C.get_or_init(|| {
        RULES
            .iter()
            .map(|r| (Regex::new(r.re).expect("security rule compiles"), r))
            .collect()
    })
}

/// Values that are obviously placeholders, not secrets.
fn placeholder(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    [
        "example",
        "xxxx",
        "dummy",
        "placeholder",
        "changeme",
        "your_",
        "your-",
        "<",
        "${",
        "{{",
        "redacted",
        "fake",
        "sample",
    ]
    .iter()
    .any(|p| l.contains(p))
}

fn ext_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name == "dockerfile" || name.starts_with("dockerfile.") {
        return "dockerfile".into();
    }
    name.rsplit_once('.')
        .map(|(_, e)| e.to_string())
        .unwrap_or_default()
}

/// Lockfiles and generated files are not scanned for code patterns or secrets.
fn skip_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "go.sum"
            | "poetry.lock"
            | "Pipfile.lock"
            | "composer.lock"
    ) || name.ends_with(".min.js")
        || name.ends_with(".map")
        || name.ends_with(".svg")
}

fn mask(line: &str) -> String {
    let t = line.trim();
    let t = crate::text::truncate_utf8(t, 200);
    // Mask anything secret-shaped: keep 4 leading chars of long tokens.
    static TOK: OnceLock<Regex> = OnceLock::new();
    let re = TOK.get_or_init(|| Regex::new(r"[A-Za-z0-9_\-+/=]{16,}").unwrap());
    re.replace_all(t, |c: &regex::Captures| {
        let s = &c[0];
        format!("{}…", &s[..4])
    })
    .into_owned()
}

/// First line of a Rust `#[cfg(test)]` module in `text` (inline unit tests), if any.
pub fn rust_test_start(text: &str) -> Option<u32> {
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if l.trim() == "#[cfg(test)]"
            && lines[i + 1..].iter().take(3).any(|n| {
                let n = n.trim_start();
                n.starts_with("mod ") || n.starts_with("pub mod ")
            })
        {
            return Some(i as u32 + 1);
        }
    }
    None
}

/// Findings on the added lines `(line number, text)` of `path`.
pub fn scan_added(path: &str, added: &[(u32, &str)]) -> Vec<SecFinding> {
    scan_added_with(path, added, None)
}

/// [`scan_added`], with lines from `test_from` on treated as test code (a Rust `#[cfg(test)]`
/// module inside a source file).
pub fn scan_added_with(
    path: &str,
    added: &[(u32, &str)],
    test_from: Option<u32>,
) -> Vec<SecFinding> {
    if skip_file(path) {
        return vec![];
    }
    let ext = ext_of(path);
    let test_file = crate::radar::is_test_path(path);
    let mut out = vec![];
    for (n, text) in added {
        let test = test_file || test_from.is_some_and(|t| *n >= t);
        if text.len() > 4000 {
            continue; // minified or generated
        }
        for (re, r) in compiled() {
            if !r.exts.is_empty() && !r.exts.contains(&ext.as_str()) {
                continue;
            }
            // Test code: fake secrets are common, risky calls are the point of some tests.
            if test && r.category != "secret" {
                continue;
            }
            if !re.is_match(text) {
                continue;
            }
            if r.category == "secret" && placeholder(text) {
                continue;
            }
            if r.id == "py.yaml-load"
                && (text.contains("SafeLoader")
                    || text.contains("safe_load")
                    || text.contains("CSafeLoader"))
            {
                continue;
            }
            let severity = if test && r.category == "secret" {
                "low"
            } else {
                r.severity
            };
            out.push(SecFinding {
                rule: r.id,
                category: r.category,
                severity,
                title: r.title,
                detail: r.detail,
                path: path.to_string(),
                line: *n,
                excerpt: mask(text),
            });
            break; // one finding per line: the first (most specific) rule wins
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(path: &str, lines: &[&str]) -> Vec<&'static str> {
        let added: Vec<(u32, &str)> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u32 + 1, *l))
            .collect();
        scan_added(path, &added)
            .into_iter()
            .map(|f| f.rule)
            .collect()
    }

    #[test]
    fn secrets_are_found_and_masked() {
        let f = scan_added(
            "src/config.rs",
            &[(3, "let key = \"AKIAABCDEFGHIJKLMNOP\";")],
        );
        assert_eq!(f[0].rule, "secret.aws-key");
        assert_eq!(f[0].severity, "critical");
        assert!(
            !f[0].excerpt.contains("ABCDEFGHIJKLMNOP"),
            "{}",
            f[0].excerpt
        );
        assert_eq!(
            rules("app.py", &["password = \"hunter2hunter2hunter2\""]),
            vec!["secret.assignment"]
        );
        assert!(
            rules("app.py", &["password = \"your_password_here_please\""]).is_empty(),
            "placeholder"
        );
        assert!(
            rules("docs/aws.md", &["AKIAIOSFODNN7EXAMPLE"]).is_empty(),
            "the AWS docs example"
        );
        // Test code: still reported, as low.
        let t = scan_added(
            "tests/it.rs",
            &[(
                1,
                "const T: &str = \"ghp_123456789012345678901234567890123456\";",
            )],
        );
        assert_eq!(t[0].severity, "low");
        assert!(rules("Cargo.lock", &["checksum = \"AKIAABCDEFGHIJKLMNOP\""]).is_empty());
    }

    #[test]
    fn code_rules_by_language() {
        assert_eq!(
            rules("web/a.js", &["el.innerHTML = userInput;"]),
            vec!["js.html-sink"]
        );
        assert_eq!(
            rules("web/a.ts", &["const f = eval(code);"]),
            vec!["js.eval"]
        );
        assert!(rules("web/a.js", &["const evaluate = (x) => x;"]).is_empty());
        assert_eq!(
            rules("svc.py", &["subprocess.run(cmd, shell=True)"]),
            vec!["py.shell-true"]
        );
        assert_eq!(
            rules("svc.py", &["data = yaml.load(f)"]),
            vec!["py.yaml-load"]
        );
        assert!(rules("svc.py", &["data = yaml.load(f, Loader=yaml.SafeLoader)"]).is_empty());
        assert_eq!(
            rules("svc.py", &["requests.get(url, verify=False)"]),
            vec!["py.tls-off"]
        );
        assert_eq!(
            rules("main.go", &["tls.Config{InsecureSkipVerify: true}"]),
            vec!["go.tls-off"]
        );
        assert_eq!(
            rules(
                "db.py",
                &["cur.execute(\"SELECT * FROM users WHERE id = %s\" % uid)"]
            ),
            vec!["sql.concat"]
        );
        assert_eq!(
            rules("q.js", &["db.query(`SELECT * FROM t WHERE id = ${id}`)"]),
            vec!["sql.concat"]
        );
        assert_eq!(
            rules("install.sh", &["curl -fsSL https://x.sh | bash"]),
            vec!["pipe-to-shell"]
        );
        assert_eq!(
            rules(
                ".github/workflows/ci.yml",
                &["on:", "  pull_request_target:"]
            ),
            vec!["gha.pr-target"]
        );
        assert_eq!(
            rules(
                ".github/workflows/ci.yml",
                &["run: echo \"${{ github.event.pull_request.title }}\""]
            ),
            vec!["gha.injection"]
        );
        assert!(
            rules("tests/test_x.py", &["eval(x)"]).is_empty(),
            "code rules skip tests"
        );
        assert_eq!(
            rules("c.rs", &[".danger_accept_invalid_certs(true)"]),
            vec!["rs.tls-off"]
        );
        assert_eq!(
            rules("a.py", &["os.chmod(path, 0o777)"]),
            vec!["world-writable"]
        );
        assert!(
            rules("a.rs", &["let bits = mode & 0o777;"]).is_empty(),
            "a mask is not a chmod"
        );
        assert!(
            rules("web/src/mock/server.js", &["el.innerHTML = x;"]).is_empty(),
            "mock data"
        );
    }

    #[test]
    fn rust_inline_test_modules_count_as_tests() {
        let text = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    const K: &str = \"AKIAABCDEFGHIJKLMNOP\";\n}\n";
        assert_eq!(rust_test_start(text), Some(3));
        let line = "    const K: &str = \"AKIAABCDEFGHIJKLMNOP\";";
        let f = scan_added_with("src/lib.rs", &[(5, line)], rust_test_start(text));
        assert_eq!(f[0].severity, "low");
        assert!(rust_test_start("#[cfg(test)]\nuse x;\nfn y() {}\n").is_none());
    }
}

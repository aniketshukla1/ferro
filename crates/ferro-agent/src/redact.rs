//! Secret redaction (B5, Appendix E) + `ai.neverSend` globs.
//! Applied to every byte sent to any provider: tool outputs are scanned
//! before they re-enter the prompt, and file reads for never-send paths are
//! refused. Replacement is a fixed token so redacted length leaks nothing.

use std::sync::OnceLock;

pub const REDACTED: &str = "[REDACTED]";

fn patterns() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Appendix E, minimum set. Private-key block uses (?s) so `.`
        // spans newlines; `sk-ant-` precedes bare `sk-` in the alternation.
        let pat = concat!(
            r"(?s)",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----|",
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b|",
            r"\bgh[pousr]_[A-Za-z0-9]{36,}|",
            r"\bgithub_pat_[A-Za-z0-9_]{50,}|",
            r"\bxox[abprs]-[A-Za-z0-9-]{10,}|",
            r"\bsk-ant-[A-Za-z0-9_-]{20,}|\bsk-[A-Za-z0-9]{20,}|",
            r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
        );
        regex::Regex::new(pat).expect("redaction patterns compile")
    })
}

/// Replace every Appendix E secret in `text` with `[REDACTED]`.
/// Returns the redacted text and the number of replacements.
pub fn redact_text(text: &str) -> (String, usize) {
    let re = patterns();
    let mut n = 0;
    let out = re.replace_all(text, |_: &regex::Captures| {
        n += 1;
        REDACTED
    });
    (out.into_owned(), n)
}

/// True when `text` contains no redactable secret.
pub fn is_clean(text: &str) -> bool {
    !patterns().is_match(text)
}

/// Build a matcher for `ai.neverSend` globs (same semantics as search:
/// `literal_separator(true)` so `*` never crosses `/`).
pub fn never_send_matcher(globs: &[String]) -> globset::GlobSet {
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        // Bare filenames (`.env*`) must match at any depth, mirroring
        // gitignore semantics used elsewhere in ferro.
        let candidates: Vec<String> = if g.contains('/') {
            vec![g.clone()]
        } else {
            vec![g.clone(), format!("**/{g}"), format!("**/{g}/**")]
        };
        for c in candidates {
            if let Ok(glob) = globset::GlobBuilder::new(&c)
                .literal_separator(true)
                .build()
            {
                b.add(glob);
            }
        }
    }
    b.build().unwrap_or_else(|_| globset::GlobSet::empty())
}

/// True when a workspace-relative path must never be sent to a provider.
pub fn is_never_send(matcher: &globset::GlobSet, rel: &str) -> bool {
    let rel = rel.trim().trim_start_matches('/');
    matcher.is_match(rel)
        || rel
            .rsplit('/')
            .next()
            .is_some_and(|base| matcher.is_match(base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appendix_e_corpus_is_redacted() {
        let cases = [
            "key AKIAIOSFODNN7EXAMPLE here",
            "key ASIAIOSFODNN7EXAMPLE here",
            "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----",
            "token ghp_123456789012345678901234567890123456 here",
            "token github_pat_12345678901234567890123456789012345678901234567890 here",
            "token xoxb-123456789012-abcdefghij here",
            "key sk-ant-12345678901234567890 here",
            "key sk-12345678901234567890 here",
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWI6IjEifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJVadQssw5c here",
        ];
        for c in cases {
            let (out, n) = redact_text(c);
            assert_eq!(n, 1, "{c}");
            assert!(!out.contains("AKIA") || c.contains("AKIA") && out.contains(REDACTED));
            assert!(is_clean(&out), "{out}");
            // No recognizable secret fragment survives.
            for frag in c.split_whitespace().filter(|w| w.len() > 12) {
                if frag == "here" || frag.starts_with("key") || frag.starts_with("token") {
                    continue;
                }
                assert!(!out.contains(frag), "{frag} survived in {out}");
            }
        }
    }

    #[test]
    fn env_and_key_files_never_send() {
        let globs: Vec<String> = [
            ".env*",
            "**/*.pem",
            "**/*.key",
            "**/id_rsa*",
            "**/*.p12",
            "**/secrets/**",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let m = never_send_matcher(&globs);
        for p in [
            ".env",
            ".env.local",
            "sub/.env",
            "tls/cert.pem",
            "tls/cert.key",
            "home/.ssh/id_rsa",
            "home/.ssh/id_rsa.pub",
            "app/key.p12",
            "a/secrets/token.txt",
        ] {
            assert!(is_never_send(&m, p), "{p}");
        }
        for p in ["src/main.rs", "Cargo.toml", "docs/keys.md"] {
            assert!(!is_never_send(&m, p), "{p}");
        }
    }

    #[test]
    fn redaction_is_idempotent_and_counted() {
        let (once, n1) = redact_text("a AKIAIOSFODNN7EXAMPLE b AKIAIOSFODNN7EXAMPLE");
        assert_eq!(n1, 2);
        let (twice, n2) = redact_text(&once);
        assert_eq!(n2, 0);
        assert_eq!(once, twice);
    }
}

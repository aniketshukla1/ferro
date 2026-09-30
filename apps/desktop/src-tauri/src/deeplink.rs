//! `ferro://open?pr=<url>` handling.
//!
//! The operating system delivers this string untrusted. Validation runs
//! before anything is sent to the local server, a browser, or a shell.
//! The only URL that leaves this module is a canonical `https` forge URL
//! rebuilt from parsed host, owner, repo, and number.

use ferro_forge::{parse_mr_url, parse_pr_url};

const MAX_LINK_LEN: usize = 4096;
const MAX_TARGET_LEN: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Malformed,
    UnsupportedScheme,
    FileTarget,
    ShellMetacharacter,
    NotForge,
}

impl Reject {
    pub fn message(self) -> &'static str {
        match self {
            Reject::Malformed => "This ferro link is malformed. Ferro did not open it.",
            Reject::UnsupportedScheme => {
                "This link does not use http or https. Ferro did not open it."
            }
            Reject::FileTarget => "File links are not opened. Ferro did not open it.",
            Reject::ShellMetacharacter => {
                "This link contains shell characters. Ferro did not open it."
            }
            Reject::NotForge => {
                "This is not a GitHub pull request or GitLab merge request URL. Ferro did not open it."
            }
        }
    }
}

/// What a validated link should do. `start_instance` is true only when no
/// Ferro process is running; the caller starts one and then opens
/// `canonical_url`. Rejected links never produce a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPlan {
    pub canonical_url: String,
    pub start_instance: bool,
}

pub fn plan_open(raw: &str, instance_running: bool) -> Result<OpenPlan, Reject> {
    let canonical_url = validate_ferro_open(raw)?;
    Ok(OpenPlan {
        canonical_url,
        start_instance: !instance_running,
    })
}

/// Parse `ferro://open?pr=<url>` and return the canonical forge URL.
pub fn validate_ferro_open(raw: &str) -> Result<String, Reject> {
    let raw = trim_wrapping_quotes(raw.trim());
    if raw.is_empty() || raw.len() > MAX_LINK_LEN {
        return Err(Reject::Malformed);
    }
    let Ok(url) = url::Url::parse(raw) else {
        return Err(if has_shell_meta(raw) {
            Reject::ShellMetacharacter
        } else {
            Reject::Malformed
        });
    };
    if !url.scheme().eq_ignore_ascii_case("ferro")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.host_str() != Some("open")
    {
        return Err(Reject::Malformed);
    }
    let path = url.path();
    if path != "/" && !path.is_empty() {
        return Err(Reject::Malformed);
    }
    let Some(query) = url.query() else {
        return Err(Reject::Malformed);
    };
    let prs: Vec<String> = url::form_urlencoded::parse(query.as_bytes())
        .filter(|(k, _)| k == "pr")
        .map(|(_, v)| v.into_owned())
        .collect();
    let [pr] = prs.as_slice() else {
        return Err(Reject::Malformed);
    };
    if pr.is_empty() {
        return Err(Reject::Malformed);
    }
    canonical_forge_url(pr)
}

/// Rebuild a GitHub PR or GitLab MR URL. The returned string is not the
/// input; it is `ForgeRef::html_url`.
pub fn canonical_forge_url(candidate: &str) -> Result<String, Reject> {
    if candidate.is_empty() || candidate.len() > MAX_TARGET_LEN {
        return Err(Reject::Malformed);
    }
    let decoded = fully_decode(candidate);
    if decoded.contains('\u{FFFD}') {
        return Err(Reject::Malformed);
    }
    if has_shell_meta(candidate) || has_shell_meta(&decoded) {
        return Err(Reject::ShellMetacharacter);
    }
    let parsed = match url::Url::parse(decoded.trim()) {
        Ok(parsed) => parsed,
        Err(_) => {
            if decoded.trim().to_ascii_lowercase().starts_with("file:") {
                return Err(Reject::FileTarget);
            }
            return Err(Reject::Malformed);
        }
    };
    if parsed.scheme().eq_ignore_ascii_case("file") {
        return Err(Reject::FileTarget);
    }
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Reject::UnsupportedScheme);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Reject::NotForge);
    }
    let forge = parse_pr_url(decoded.trim())
        .or_else(|| parse_mr_url(decoded.trim()))
        .ok_or(Reject::NotForge)?;
    let canonical = forge.html_url();
    if has_shell_meta(&canonical) || !canonical.starts_with("https://") {
        return Err(Reject::ShellMetacharacter);
    }
    Ok(canonical)
}

/// POST `/api/v1/pr/open` on the desktop app's own loopback server.
/// Re-validates `canonical_url` and will not send anything that fails.
pub async fn post_validated_pr(
    origin: &str,
    token: &str,
    canonical_url: &str,
) -> Result<(), String> {
    let canonical =
        canonical_forge_url(canonical_url).map_err(|reject| reject.message().to_string())?;
    if !is_loopback_http_origin(origin) {
        return Err(
            "Ferro will not send a pull request link anywhere but its own local server.".into(),
        );
    }
    if token.is_empty()
        || token
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err("Ferro could not authenticate to its local server.".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Ferro could not reach its local server.".to_string())?;
    let response = client
        .post(format!("{origin}/api/v1/pr/open"))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .json(&serde_json::json!({ "url": canonical }))
        .send()
        .await
        .map_err(|_| "Ferro could not reach its local server.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Ferro could not open that pull request ({}).",
            response.status()
        ));
    }
    Ok(())
}

pub fn is_loopback_http_origin(origin: &str) -> bool {
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_none()
    {
        return false;
    }
    if url.path() != "/" && !url.path().is_empty() {
        return false;
    }
    matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
}

fn trim_wrapping_quotes(raw: &str) -> &str {
    raw.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(raw)
}

/// Characters that must never be handed to a shell, even inside one argument.
fn has_shell_meta(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                ' ' | ';'
                    | '|'
                    | '&'
                    | '$'
                    | '`'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '!'
                    | '*'
                    | '?'
                    | '~'
                    | '\''
                    | '"'
                    | '\\'
                    | '#'
                    | '['
                    | ']'
                    | '^'
            )
    })
}

fn fully_decode(input: &str) -> String {
    let mut cur = input.to_string();
    for _ in 0..4 {
        let next = percent_decode_once(&cur);
        if next == cur {
            break;
        }
        cur = next;
    }
    cur
}

fn percent_decode_once(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(value) = u8::from_str_radix(hex, 16) {
                    out.push(value);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ForwardLog {
        urls: Vec<String>,
        started: bool,
    }

    impl ForwardLog {
        fn apply(&mut self, raw: &str, instance_running: bool) -> Result<(), Reject> {
            let plan = plan_open(raw, instance_running)?;
            if plan.start_instance {
                self.started = true;
            }
            self.urls.push(plan.canonical_url);
            Ok(())
        }
    }

    #[test]
    fn opens_https_forge_url_in_a_running_instance() {
        let mut log = ForwardLog::default();
        log.apply(
            "ferro://open?pr=https://github.com/owner/repo/pull/123",
            true,
        )
        .unwrap();
        assert_eq!(
            log.urls,
            vec!["https://github.com/owner/repo/pull/123".to_string()]
        );
        assert!(!log.started);
    }

    #[test]
    fn starts_an_instance_when_none_is_running_then_opens() {
        let mut log = ForwardLog::default();
        log.apply(
            "ferro://open?pr=https://gitlab.com/group/proj/-/merge_requests/42",
            false,
        )
        .unwrap();
        assert!(log.started);
        assert_eq!(
            log.urls,
            vec!["https://gitlab.com/group/proj/-/merge_requests/42".to_string()]
        );
    }

    #[test]
    fn accepts_percent_encoded_and_http_targets_as_canonical_https() {
        let encoded = validate_ferro_open(
            "ferro://open?pr=https%3A%2F%2Fgithub.com%2Fowner%2Frepo%2Fpull%2F9",
        )
        .unwrap();
        assert_eq!(encoded, "https://github.com/owner/repo/pull/9");
        let http =
            validate_ferro_open("ferro://open?pr=http://github.com/owner/repo/pull/9").unwrap();
        assert_eq!(http, "https://github.com/owner/repo/pull/9");
    }

    #[test]
    fn rejects_malformed_url_without_forwarding() {
        for raw in [
            "ferro://open",
            "ferro://open?pr=",
            "ferro://%%%",
            "::::",
            "ferro://nope?pr=https://github.com/owner/repo/pull/1",
            "ferro://open?pr=notaurl",
            "https://github.com/owner/repo/pull/1",
        ] {
            let mut log = ForwardLog::default();
            let err = log.apply(raw, false).unwrap_err();
            assert_eq!(err, Reject::Malformed, "{raw}");
            assert!(log.urls.is_empty(), "{raw}");
            assert!(!log.started, "{raw}");
            assert!(!err.message().is_empty());
        }
    }

    #[test]
    fn rejects_non_http_scheme_without_forwarding() {
        for raw in [
            "ferro://open?pr=ftp://github.com/owner/repo/pull/1",
            "ferro://open?pr=ssh://github.com/owner/repo/pull/1",
            "ferro://open?pr=data:text/plain,hello",
        ] {
            let mut log = ForwardLog::default();
            let err = log.apply(raw, true).unwrap_err();
            assert_eq!(err, Reject::UnsupportedScheme, "{raw}");
            assert!(log.urls.is_empty(), "{raw}");
            assert!(!log.started, "{raw}");
        }
    }

    #[test]
    fn rejects_file_target_without_forwarding() {
        for raw in [
            "ferro://open?pr=file:///etc/passwd",
            "ferro://open?pr=FILE:///tmp/secret",
            "ferro://open?pr=file%3A%2F%2F%2Fetc%2Fpasswd",
        ] {
            let mut log = ForwardLog::default();
            let err = log.apply(raw, true).unwrap_err();
            assert_eq!(err, Reject::FileTarget, "{raw}");
            assert!(log.urls.is_empty(), "{raw}");
            assert!(!err.message().is_empty());
        }
    }

    #[test]
    fn rejects_shell_metacharacter_payload_without_forwarding() {
        for raw in [
            "ferro://open?pr=https://github.com/o/r/pull/1;rm%20-rf%20/",
            "ferro://open?pr=https://github.com/o/r/pull/1%24%28id%29",
            "ferro://open?pr=https://github.com/o/r/pull/1%60id%60",
            "ferro://open?pr=https://github.com/o/r/pull/1%7Cid",
            "ferro://open?pr=https://github.com/o/r/pull/1%26%26true",
            "ferro://open?pr=https://github.com/o/r/pull/1%0Awhoami",
        ] {
            let mut log = ForwardLog::default();
            let err = log.apply(raw, false).unwrap_err();
            assert_eq!(err, Reject::ShellMetacharacter, "{raw}");
            assert!(log.urls.is_empty(), "{raw}");
            assert!(!log.started, "{raw}");
            assert!(err.message().contains("shell"));
        }
    }

    #[test]
    fn splash_uses_app_tokens_and_svg_mark() {
        let html = include_str!("../../splash/index.html");
        let lower = html.to_ascii_lowercase();
        assert!(
            !lower.contains("#1a1a1a"),
            "splash still hard-codes the legacy background"
        );
        assert!(!html.contains(">F<"), "splash still uses a text glyph");
        assert!(html.contains("--bg-chrome"));
        assert!(html.contains("#0f0f11"), "graphite --bg-chrome");
        assert!(html.contains("#f6f6f7"), "porcelain --bg-chrome");
        assert!(html.contains("prefers-color-scheme: light"));
        assert!(html.contains("<rect "), "forge mark is an SVG, not text");
        assert!(html.contains("#ff8c2e"));
        assert!(html.contains("prefers-reduced-motion"));
    }

    #[tokio::test]
    async fn post_does_not_send_rejected_or_non_loopback_targets() {
        let file = post_validated_pr("http://127.0.0.1:9", "token", "file:///etc/passwd")
            .await
            .unwrap_err();
        assert!(file.contains("File links"));

        let shell = post_validated_pr(
            "http://127.0.0.1:9",
            "token",
            "https://github.com/o/r/pull/1;id",
        )
        .await
        .unwrap_err();
        assert!(shell.contains("shell"));

        let remote = post_validated_pr(
            "https://evil.example/api",
            "token",
            "https://github.com/owner/repo/pull/1",
        )
        .await
        .unwrap_err();
        assert!(remote.contains("local server"));

        let token_in_origin = post_validated_pr(
            "http://127.0.0.1:9/?token=secret",
            "token",
            "https://github.com/owner/repo/pull/1",
        )
        .await
        .unwrap_err();
        assert!(token_in_origin.contains("local server"));
    }

    #[tokio::test]
    async fn post_sends_the_canonical_url_to_loopback_only() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
        let captured = seen.clone();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            *captured.lock().await = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = br#"{"job":{"id":"j","kind":"pr.open"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.write_all(body).await.unwrap();
        });

        post_validated_pr(
            &format!("http://127.0.0.1:{port}"),
            "desktop-test-token",
            "https://github.com/owner/repo/pull/1",
        )
        .await
        .unwrap();

        let req = seen.lock().await.clone();
        assert!(req.starts_with("POST /api/v1/pr/open HTTP/1.1"), "{req}");
        assert!(
            req.contains("authorization: Bearer desktop-test-token")
                || req.contains("Authorization: Bearer desktop-test-token"),
            "{req}"
        );
        assert!(
            req.contains("\"url\":\"https://github.com/owner/repo/pull/1\""),
            "{req}"
        );
        assert!(!req.contains("file:"), "{req}");
        assert!(!req.contains(';'), "{req}");
    }
}

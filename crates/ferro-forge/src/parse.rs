//! PR/MR URL parsing: github.com and Enterprise hosts, `pull` URLs.
//! (GitLab MR URLs land in B4b.) Host comparison is case-insensitive;
//! `.git` suffixes and trailing slashes are tolerated.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRef {
    /// Provider host: `github.com` or a GHE hostname.
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl ForgeRef {
    /// `owner/repo#number` display name (API.md workspace name in PR mode).
    pub fn name(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }

    pub fn is_dotcom(&self) -> bool {
        self.host.eq_ignore_ascii_case("github.com")
    }

    /// REST base: `https://api.github.com` or `https://<host>/api/v3`.
    pub fn api_base(&self) -> String {
        if self.is_dotcom() {
            "https://api.github.com".into()
        } else {
            format!("https://{}/api/v3", self.host)
        }
    }

    /// GraphQL endpoint.
    pub fn graphql_url(&self) -> String {
        if self.is_dotcom() {
            "https://api.github.com/graphql".into()
        } else {
            format!("https://{}/api/graphql", self.host)
        }
    }

    /// Human URL back to the PR.
    pub fn html_url(&self) -> String {
        format!(
            "https://{}/{}/{}/pull/{}",
            self.host, self.owner, self.repo, self.number
        )
    }

    /// Clone URL for git (token goes via env extraheader, never argv).
    pub fn clone_url(&self) -> String {
        format!("https://{}/{}/{}.git", self.host, self.owner, self.repo)
    }
}

/// A DNS hostname with an optional port: letters, digits, `-` and `.`,
/// at least one dot, no empty labels. Rejects userinfo (`github.com@evil`),
/// which would send requests (and the token) to another host.
fn valid_host(host: &str) -> bool {
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if port.is_some_and(|p| p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Owner and repo names: GitHub allows letters, digits, `-`, `_` and `.`;
/// `.` and `..` alone are path segments, not names. They end up in API
/// URLs and cache paths, so nothing else passes.
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Parse `https://<host>/<owner>/<repo>/pull/<n>` (scheme optional).
pub fn parse_pr_url(s: &str) -> Option<ForgeRef> {
    let s = s.trim().trim_end_matches('/');
    let without_scheme = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let (host, rest) = without_scheme.split_once('/')?;
    if !valid_host(host) {
        return None;
    }
    let mut parts = rest.split('/');
    let owner = parts.next()?;
    let mut repo = parts.next()?;
    if parts.next()? != "pull" {
        return None;
    }
    let number: u64 = parts.next()?.parse().ok()?;
    if number == 0 {
        return None;
    }
    repo = repo.strip_suffix(".git").unwrap_or(repo);
    if !valid_name(owner) || !valid_name(repo) {
        return None;
    }
    // Reject trailing junk (`/pull/1/files` is not a PR URL).
    if parts.next().is_some() {
        return None;
    }
    Some(ForgeRef {
        host: host.to_lowercase(),
        owner: owner.into(),
        repo: repo.into(),
        number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotcom_and_enterprise() {
        let r = parse_pr_url("https://github.com/owner/repo/pull/123").unwrap();
        assert_eq!(r.api_base(), "https://api.github.com");
        assert!(r.is_dotcom());
        let e = parse_pr_url("https://ghe.corp.example/o/r/pull/7/").unwrap();
        assert_eq!(e.host, "ghe.corp.example");
        assert_eq!(e.api_base(), "https://ghe.corp.example/api/v3");
        assert_eq!(e.graphql_url(), "https://ghe.corp.example/api/graphql");
        assert_eq!(e.name(), "o/r#7");
        assert_eq!(e.clone_url(), "https://ghe.corp.example/o/r.git");
    }

    #[test]
    fn rejects_non_pr() {
        for bad in [
            "https://github.com/owner/repo/issues/1",
            "https://github.com/owner/repo/pull/0",
            "https://github.com/owner/repo/pull/1/files",
            "https://github.com/owner/pull/1",
            "/some/path",
            "not a url",
            "https://github.com//repo/pull/1",
            "https://github.com/o/r/pull/abc",
            // Userinfo would route requests (and the token) elsewhere.
            "https://github.com@evil.example/o/r/pull/1",
            "https://evil.example:1@github.com/o/r/pull/1",
            // Dot segments and odd characters never reach URLs or paths.
            "https://github.com/../r/pull/1",
            "https://github.com/o/../pull/1",
            "https://github.com/o/r%2F..%2Fx/pull/1",
            "https://github.com/o?x=1/r/pull/1",
            "https://-bad.example/o/r/pull/1",
            "https://github.com:/o/r/pull/1",
        ] {
            assert!(parse_pr_url(bad).is_none(), "{bad}");
        }
        let p = parse_pr_url("https://ghe.corp.example:8443/my-org/my_repo.rs/pull/2").unwrap();
        assert_eq!(
            (p.host.as_str(), p.owner.as_str(), p.repo.as_str()),
            ("ghe.corp.example:8443", "my-org", "my_repo.rs")
        );
    }
}

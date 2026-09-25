//! PR/MR URL parsing: github.com and Enterprise hosts, `pull` URLs;
//! gitlab.com and self-hosted instances, `merge_requests` URLs.
//! Host comparison is case-insensitive; `.git` suffixes and trailing
//! slashes are tolerated.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    GitHub,
    GitLab,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::GitLab => "gitlab",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRef {
    pub provider: Provider,
    /// Provider host: `github.com`, a GHE hostname, `gitlab.com` or self-hosted.
    pub host: String,
    /// `owner` (GitHub / first namespace?) — for GitLab this is the full
    /// namespace path; `repo` is the project slug.
    pub owner: String,
    pub repo: String,
    /// PR number (GitHub) or MR IID (GitLab).
    pub number: u64,
}

impl ForgeRef {
    /// `owner/repo#number` display name (API.md workspace name in PR mode).
    pub fn name(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }

    pub fn is_dotcom(&self) -> bool {
        self.provider == Provider::GitHub && self.host.eq_ignore_ascii_case("github.com")
    }

    /// REST base: GitHub `https://api.github.com` / GHE `/api/v3`;
    /// GitLab `https://<host>/api/v4`.
    pub fn api_base(&self) -> String {
        match self.provider {
            Provider::GitHub => {
                if self.is_dotcom() {
                    "https://api.github.com".into()
                } else {
                    format!("https://{}/api/v3", self.host)
                }
            }
            Provider::GitLab => format!("https://{}/api/v4", self.host),
        }
    }

    /// GraphQL endpoint (GitHub only; GitLab has none for reviews).
    pub fn graphql_url(&self) -> String {
        if self.is_dotcom() {
            "https://api.github.com/graphql".into()
        } else {
            format!("https://{}/api/graphql", self.host)
        }
    }

    /// Full namespace/project path (`owner/repo`, subgroups included).
    pub fn project_path(&self) -> String {
        if self.owner.is_empty() {
            self.repo.clone()
        } else {
            format!("{}/{}", self.owner, self.repo)
        }
    }

    /// URL-encoded project path for GitLab `:id` params.
    pub fn encoded_project(&self) -> String {
        url_encode(&self.project_path())
    }

    /// Human URL back to the PR/MR.
    pub fn html_url(&self) -> String {
        match self.provider {
            Provider::GitHub => format!(
                "https://{}/{}/{}/pull/{}",
                self.host, self.owner, self.repo, self.number
            ),
            Provider::GitLab => format!(
                "https://{}/{}/-/merge_requests/{}",
                self.host,
                self.project_path(),
                self.number
            ),
        }
    }

    /// Clone URL for git (token goes via env extraheader, never argv).
    pub fn clone_url(&self) -> String {
        format!("https://{}/{}.git", self.host, self.project_path())
    }

    /// Remote head ref for fetching: GitHub `pull/<n>/head`,
    /// GitLab `merge-requests/<iid>/head`.
    pub fn pull_ref(&self) -> String {
        match self.provider {
            Provider::GitHub => format!("pull/{}/head", self.number),
            Provider::GitLab => format!("merge-requests/{}/head", self.number),
        }
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
        provider: Provider::GitHub,
        host: host.to_lowercase(),
        owner: owner.into(),
        repo: repo.into(),
        number,
    })
}

/// Parse `https://<host>/<namespace...>/<project>/-/merge_requests/<iid>`
/// (scheme optional). Namespace may nest (subgroups).
pub fn parse_mr_url(s: &str) -> Option<ForgeRef> {
    let s = s.trim().trim_end_matches('/');
    let without_scheme = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let (host, rest) = without_scheme.split_once('/')?;
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    let mut parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 4 {
        return None;
    }
    let iid: u64 = parts.pop()?.parse().ok()?;
    if iid == 0 {
        return None;
    }
    if parts.pop()? != "merge_requests" {
        return None;
    }
    if parts.pop()? != "-" {
        return None;
    }
    let repo = parts.pop()?.trim();
    if repo.is_empty() {
        return None;
    }
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let namespace = parts.join("/");
    if namespace.is_empty() {
        return None;
    }
    Some(ForgeRef {
        provider: Provider::GitLab,
        host: host.to_lowercase(),
        owner: namespace,
        repo: repo.into(),
        number: iid,
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

    #[test]
    fn github_pull_ref() {
        let r = parse_pr_url("https://github.com/o/r/pull/9").unwrap();
        assert_eq!(r.provider, Provider::GitHub);
        assert_eq!(r.pull_ref(), "pull/9/head");
    }

    #[test]
    fn gitlab_mr_urls() {
        let r = parse_mr_url("https://gitlab.com/group/proj/-/merge_requests/42").unwrap();
        assert_eq!(r.provider, Provider::GitLab);
        assert_eq!(r.api_base(), "https://gitlab.com/api/v4");
        assert_eq!(r.encoded_project(), "group%2Fproj");
        assert_eq!(
            r.html_url(),
            "https://gitlab.com/group/proj/-/merge_requests/42"
        );
        assert_eq!(r.clone_url(), "https://gitlab.com/group/proj.git");
        assert_eq!(r.pull_ref(), "merge-requests/42/head");
        let sub = parse_mr_url("https://git.corp.example/a/b/c/-/merge_requests/7/").unwrap();
        assert_eq!((sub.owner.as_str(), sub.repo.as_str()), ("a/b", "c"));
        assert_eq!(sub.encoded_project(), "a%2Fb%2Fc");
        assert_eq!(sub.pull_ref(), "merge-requests/7/head");
        // GitHub pull URLs never parse as MRs and vice versa.
        assert!(parse_mr_url("https://github.com/o/r/pull/1").is_none());
        assert!(parse_pr_url("https://gitlab.com/group/proj/-/merge_requests/42").is_none());
        for bad in [
            "https://gitlab.com/group/proj/-/merge_requests/0",
            "https://gitlab.com/group/-/merge_requests/1",
            "https://gitlab.com/group/proj/merge_requests/1",
            "https://gitlab.com/group/proj/-/merge_requests/abc",
        ] {
            assert!(parse_mr_url(bad).is_none(), "{bad}");
        }
    }
}

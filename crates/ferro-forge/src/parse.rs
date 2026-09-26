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

/// Parse `https://<host>/<owner>/<repo>/pull/<n>` (scheme optional).
pub fn parse_pr_url(s: &str) -> Option<ForgeRef> {
    let s = s.trim().trim_end_matches('/');
    let without_scheme = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let (host, rest) = without_scheme.split_once('/')?;
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim();
    let mut repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    if parts.next()? != "pull" {
        return None;
    }
    let number: u64 = parts.next()?.parse().ok()?;
    if number == 0 {
        return None;
    }
    repo = repo.strip_suffix(".git").unwrap_or(repo);
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
        ] {
            assert!(parse_pr_url(bad).is_none(), "{bad}");
        }
    }
}

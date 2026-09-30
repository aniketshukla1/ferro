//! Token resolution (B4): `GITHUB_TOKEN` → `GH_TOKEN` → `gh auth token
//! --hostname <host>` → keychain (B8); GitLab (B4b): `GITLAB_TOKEN` →
//! `GITLAB_ACCESS_TOKEN` → glab's stored token. Names are safe to log;
//! values never are. Git-over-HTTPS carries the token via `GIT_CONFIG_*`
//! env extraheader, never argv.

use super::parse::Provider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    Env,
    Gh,
    Glab,
    Keychain,
}

impl TokenSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenSource::Env => "env",
            TokenSource::Gh => "gh",
            TokenSource::Glab => "glab",
            TokenSource::Keychain => "keychain",
        }
    }
}

/// Resolve a GitHub token for `host`. Returns the token and its source.
///
/// Env tokens are host-scoped like the `gh` CLI scopes them:
/// `GITHUB_TOKEN`/`GH_TOKEN` belong to github.com, and
/// `GH_ENTERPRISE_TOKEN`/`GITHUB_ENTERPRISE_TOKEN` to the one host named by
/// `GH_HOST`. Any other host (a PR link can name any host) only gets what
/// `gh auth token --hostname <host>` holds for it — never another host's
/// token.
pub fn resolve_token(host: &str) -> Option<(String, TokenSource)> {
    let host = host.to_ascii_lowercase();
    let env_vars: &[&str] = if host == "github.com" {
        &["GITHUB_TOKEN", "GH_TOKEN"]
    } else if std::env::var("GH_HOST").is_ok_and(|h| h.trim().eq_ignore_ascii_case(&host)) {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    } else {
        &[]
    };
    for var in env_vars {
        if let Ok(t) = std::env::var(var) {
            let t = t.trim().to_string();
            if !t.is_empty() {
                return Some((t, TokenSource::Env));
            }
        }
    }
    if let Some(t) = gh_token(&host) {
        return Some((t, TokenSource::Gh));
    }
    if let Some(t) = ferro_core::credentials::github_token(&host) {
        return Some((t, TokenSource::Keychain));
    }
    None
}

/// Resolve a GitLab token for `host`. Returns the token and its source.
///
/// glab's env tokens (`GITLAB_TOKEN`, then `GITLAB_ACCESS_TOKEN`) are not
/// host-scoped in glab; here they belong to glab's default host only —
/// `GITLAB_HOST` / `GL_HOST` / `GITLAB_URI`, else gitlab.com — as the GitHub
/// env tokens belong to theirs. Any other host (an MR link can name any
/// host) only gets what glab has stored for exactly it.
pub fn resolve_gitlab_token(host: &str) -> Option<(String, TokenSource)> {
    let host = host.to_ascii_lowercase();
    if host == gitlab_default_host() {
        for var in ["GITLAB_TOKEN", "GITLAB_ACCESS_TOKEN"] {
            if let Ok(t) = std::env::var(var) {
                let t = t.trim().to_string();
                if !t.is_empty() {
                    return Some((t, TokenSource::Env));
                }
            }
        }
    }
    if let Some(t) = glab_token(&host) {
        return Some((t, TokenSource::Glab));
    }
    ferro_core::credentials::gitlab_token(&host).map(|t| (t, TokenSource::Keychain))
}

/// glab's default host, lowercased (a URL value is tolerated).
fn gitlab_default_host() -> String {
    ["GITLAB_HOST", "GL_HOST", "GITLAB_URI"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|h| !h.trim().is_empty()))
        .map(|h| {
            let h = h.trim();
            let h = h
                .strip_prefix("https://")
                .or_else(|| h.strip_prefix("http://"))
                .unwrap_or(h);
            h.trim_end_matches('/').to_ascii_lowercase()
        })
        .unwrap_or_else(|| "gitlab.com".into())
}

/// `gh`'s stored credential for exactly `host`. The env tokens are removed
/// first: gh would otherwise answer any `--hostname` with
/// `GH_ENTERPRISE_TOKEN` (or `GH_TOKEN` for github.com), bypassing the
/// host scoping above.
fn gh_token(host: &str) -> Option<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// glab's stored credential for exactly `host` (glab has no `auth token`;
/// `config get token --host` reads the per-host entry). glab answers from
/// its env tokens first, for any host, so those are removed as for gh.
fn glab_token(host: &str) -> Option<String> {
    let out = std::process::Command::new("glab")
        .args(["config", "get", "token", "--host", host])
        .env_remove("GITLAB_TOKEN")
        .env_remove("GITLAB_ACCESS_TOKEN")
        .env_remove("OAUTH_TOKEN")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // A bare token only, never a hint or help text.
    (!t.is_empty() && !t.contains(char::is_whitespace)).then_some(t)
}

/// `http.<url>.extraheader` config entry for authenticated git over HTTPS.
/// Scoped to `url`, so git sends it to that remote only. The username is
/// the provider's ([`Provider::git_user`]).
pub fn auth_config(url: &str, provider: Provider, token: &str) -> (String, String) {
    // base64 "<user>:<token>" — no token bytes in argv, only env.
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{token}", provider.git_user()));
    (
        format!("http.{url}.extraheader"),
        format!("AUTHORIZATION: Basic {creds}"),
    )
}

/// Config entries as git's `GIT_CONFIG_COUNT/KEY_n/VALUE_n` env: one set
/// per child, so auth and hardening entries must be passed together.
pub fn config_env(entries: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), entries.len().to_string())];
    for (i, (k, v)) in entries.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), k.clone()));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), v.clone()));
    }
    env
}

/// `http.<url>.extraheader` env triple for authenticated git over HTTPS.
/// Usage: extend the git command's env with all three pairs.
pub fn git_auth_env(url: &str, provider: Provider, token: &str) -> Vec<(String, String)> {
    config_env(&[auth_config(url, provider, token)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_chain() {
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("GH_TOKEN");
        // gh CLI may exist with auth; only assert the env path deterministically.
        std::env::set_var("GH_TOKEN", "  tok123  ");
        let (t, src) = resolve_token("github.com").unwrap();
        assert_eq!(t, "tok123");
        assert_eq!(src, TokenSource::Env);
        // A github.com token never goes to another host, whatever a PR
        // link names (gh's own per-host store may still answer).
        assert!(resolve_token("evil.example")
            .is_none_or(|(t, s)| t != "tok123" && s != TokenSource::Env));
        // Enterprise env tokens belong to GH_HOST only.
        std::env::set_var("GH_HOST", "ghe.corp.example");
        std::env::set_var("GH_ENTERPRISE_TOKEN", "ent456");
        assert_eq!(resolve_token("GHE.corp.example").unwrap().0, "ent456");
        assert!(resolve_token("other.example").is_none_or(|(t, _)| t != "ent456"));
        std::env::remove_var("GH_HOST");
        std::env::remove_var("GH_ENTERPRISE_TOKEN");
        std::env::remove_var("GH_TOKEN");
    }

    #[test]
    fn config_env_numbers_entries() {
        let env = config_env(&[("a.b".into(), "1".into()), ("c.d".into(), "2".into())]);
        assert_eq!(env[0], ("GIT_CONFIG_COUNT".into(), "2".into()));
        assert_eq!(env[3], ("GIT_CONFIG_KEY_1".into(), "c.d".into()));
        assert_eq!(env[4], ("GIT_CONFIG_VALUE_1".into(), "2".into()));
    }

    #[test]
    fn gitlab_env_token_is_host_scoped() {
        for v in [
            "GITLAB_HOST",
            "GL_HOST",
            "GITLAB_URI",
            "GITLAB_ACCESS_TOKEN",
        ] {
            std::env::remove_var(v);
        }
        std::env::set_var("GITLAB_TOKEN", " glpat-envtok ");
        let (t, src) = resolve_gitlab_token("GitLab.com").unwrap();
        assert_eq!((t.as_str(), src), ("glpat-envtok", TokenSource::Env));
        // An MR link can name any host; the env token never goes there
        // (glab's own per-host store may still answer for it).
        assert!(resolve_gitlab_token("evil.example").is_none_or(|(t, _)| t != "glpat-envtok"));
        // GITLAB_HOST (a URL is tolerated) moves the env token's home.
        std::env::set_var("GITLAB_HOST", "https://git.corp.example/");
        assert_eq!(
            resolve_gitlab_token("git.corp.example").unwrap().0,
            "glpat-envtok"
        );
        assert!(resolve_gitlab_token("gitlab.com").is_none_or(|(t, _)| t != "glpat-envtok"));
        std::env::remove_var("GITLAB_HOST");
        std::env::remove_var("GITLAB_TOKEN");
    }

    #[test]
    fn auth_env_shape() {
        use base64::Engine as _;
        let env = git_auth_env("https://github.com/o/r.git", Provider::GitHub, "sekret");
        assert_eq!(env[0], ("GIT_CONFIG_COUNT".into(), "1".into()));
        assert!(env[1].1.contains("extraheader"));
        assert!(!env[2].1.contains("sekret") || env[2].1.starts_with("AUTHORIZATION: Basic "));
        // GitLab OAuth tokens only authenticate as `oauth2`.
        let (_, v) = auth_config("https://gitlab.com/g/p.git", Provider::GitLab, "tok");
        let creds = v.strip_prefix("AUTHORIZATION: Basic ").unwrap();
        let plain = base64::engine::general_purpose::STANDARD
            .decode(creds)
            .unwrap();
        assert_eq!(plain, b"oauth2:tok");
    }
}

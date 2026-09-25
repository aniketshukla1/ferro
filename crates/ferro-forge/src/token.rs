//! Token resolution (B4): `GITHUB_TOKEN` → `GH_TOKEN` → `gh auth token
//! --hostname <host>` → keychain (B8). Names are safe to log; values never
//! are. Git-over-HTTPS carries the token via `GIT_CONFIG_*` env extraheader,
//! never argv.

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
    // B8 fills in the keychain lookup.
    None
}

/// Resolve a GitLab token for `host`: `GITLAB_TOKEN` first, then a
/// best-effort `glab auth token` probe (absent/old CLIs simply miss).
/// Returns the token and its source.
pub fn resolve_gitlab_token(host: &str) -> Option<(String, TokenSource)> {
    if let Ok(t) = std::env::var("GITLAB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some((t, TokenSource::Env));
        }
    }
    if let Some(t) = glab_token(host) {
        return Some((t, TokenSource::Glab));
    }
    None
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

/// `http.<url>.extraheader` config entry for authenticated git over HTTPS.
/// Scoped to `url`, so git sends it to that remote only.
pub fn auth_config(url: &str, token: &str) -> (String, String) {
    // base64 "x-access-token:<token>" — no token bytes in argv, only env.
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    (
        format!("http.{url}.extraheader"),
        format!("AUTHORIZATION: Basic {creds}"),
    )
}

fn glab_token(host: &str) -> Option<String> {
    // Newer glab CLIs print the token; older ones lack the subcommand.
    for args in [
        vec!["auth", "token", "--hostname", host],
        vec!["auth", "token", "-h", host],
    ] {
        if let Ok(out) = std::process::Command::new("glab").args(&args).output() {
            if out.status.success() {
                let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
                // glab may print hints instead of a bare token; accept only
                // plausible token shapes.
                if (t.starts_with("glpat-") || t.starts_with("gloas-") || t.len() >= 20)
                    && !t.contains(char::is_whitespace)
                {
                    return Some(t);
                }
            }
        }
    }
    None
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
pub fn git_auth_env(url: &str, token: &str) -> Vec<(String, String)> {
    config_env(&[auth_config(url, token)])
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
    fn auth_env_shape() {
        let env = git_auth_env("https://github.com/o/r.git", "sekret");
        assert_eq!(env[0], ("GIT_CONFIG_COUNT".into(), "1".into()));
        assert!(env[1].1.contains("extraheader"));
        assert!(!env[2].1.contains("sekret") || env[2].1.starts_with("AUTHORIZATION: Basic "));
    }
}

//! Token resolution (B4): `GITHUB_TOKEN` → `GH_TOKEN` → `gh auth token
//! --hostname <host>` → keychain (B8). Names are safe to log; values never
//! are. Git-over-HTTPS carries the token via `GIT_CONFIG_*` env extraheader,
//! never argv.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    Env,
    Gh,
    Keychain,
}

impl TokenSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenSource::Env => "env",
            TokenSource::Gh => "gh",
            TokenSource::Keychain => "keychain",
        }
    }
}

/// Resolve a GitHub token for `host`. Returns the token and its source.
pub fn resolve_token(host: &str) -> Option<(String, TokenSource)> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            let t = t.trim().to_string();
            if !t.is_empty() {
                return Some((t, TokenSource::Env));
            }
        }
    }
    if let Some(t) = gh_token(host) {
        return Some((t, TokenSource::Gh));
    }
    // B8 fills in the keychain lookup.
    None
}

fn gh_token(host: &str) -> Option<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// `http.<url>.extraheader` env triple for authenticated git over HTTPS.
/// Usage: extend the git command's env with all three pairs.
pub fn git_auth_env(url: &str, token: &str) -> Vec<(String, String)> {
    // base64 "x-access-token:<token>" — no token bytes in argv, only env.
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    let value = format!("AUTHORIZATION: Basic {creds}");
    vec![
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), format!("http.{url}.extraheader")),
        ("GIT_CONFIG_VALUE_0".into(), value),
    ]
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
        std::env::remove_var("GH_TOKEN");
    }

    #[test]
    fn auth_env_shape() {
        let env = git_auth_env("https://github.com/o/r.git", "sekret");
        assert_eq!(env[0], ("GIT_CONFIG_COUNT".into(), "1".into()));
        assert!(env[1].1.contains("extraheader"));
        assert!(!env[2].1.contains("sekret") || env[2].1.starts_with("AUTHORIZATION: Basic "));
    }
}

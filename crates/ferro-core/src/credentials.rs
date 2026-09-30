//! OS keychain storage for provider keys and forge tokens (B8).
//! Values never touch disk in plaintext and are not returned by any API.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use thiserror::Error;

pub const SERVICE: &str = "ferro";

/// In-process credential map for tests (`FERRO_MOCK_KEYRING=1`). The OS mock
/// keystore does not persist across `Entry::new` calls, so integration tests
/// use this instead of touching the real keychain. Debug builds only — a
/// release binary always uses the OS keychain.
static MEM_STORE: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    static FORCE_UNAVAILABLE: Cell<bool> = const { Cell::new(false) };
}

fn mem_enabled() -> bool {
    cfg!(debug_assertions) && std::env::var("FERRO_MOCK_KEYRING").as_deref() == Ok("1")
}

fn forced_unavailable() -> bool {
    cfg!(debug_assertions) && FORCE_UNAVAILABLE.with(Cell::get)
}

/// Enable the in-memory credential backend (debug/test builds only).
pub fn enable_mem_store_for_tests() {
    std::env::set_var("FERRO_MOCK_KEYRING", "1");
    MEM_STORE.lock().unwrap().clear();
}

/// Make `store` return [`CredentialError::KeychainUnavailable`] on this thread.
/// No-op in release builds.
pub fn force_keychain_unavailable_for_tests(on: bool) {
    if cfg!(debug_assertions) {
        FORCE_UNAVAILABLE.with(|flag| flag.set(on));
    }
}

/// Stable credential ids accepted by `PUT /api/v1/credentials`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialId {
    AnthropicApiKey,
    AnthropicAuthToken,
    OpenAiApiKey,
    GeminiApiKey,
    GitHubToken { host: String },
    GitLabToken { host: String },
}

impl CredentialId {
    pub fn wire_id(&self) -> String {
        match self {
            Self::AnthropicApiKey => "provider.anthropic.api_key".into(),
            Self::AnthropicAuthToken => "provider.anthropic.auth_token".into(),
            Self::OpenAiApiKey => "provider.openai.api_key".into(),
            Self::GeminiApiKey => "provider.gemini.api_key".into(),
            Self::GitHubToken { host } => format!("forge.github:{host}"),
            Self::GitLabToken { host } => format!("forge.gitlab:{host}"),
        }
    }

    pub fn parse(s: &str) -> Result<Self, CredentialError> {
        let s = s.trim();
        match s {
            "provider.anthropic.api_key" => Ok(Self::AnthropicApiKey),
            "provider.anthropic.auth_token" => Ok(Self::AnthropicAuthToken),
            "provider.openai.api_key" => Ok(Self::OpenAiApiKey),
            "provider.gemini.api_key" => Ok(Self::GeminiApiKey),
            _ if s.starts_with("forge.github:") => {
                let host = parse_host(s.strip_prefix("forge.github:").unwrap_or(""))?;
                Ok(Self::GitHubToken { host })
            }
            _ if s.starts_with("forge.gitlab:") => {
                let host = parse_host(s.strip_prefix("forge.gitlab:").unwrap_or(""))?;
                Ok(Self::GitLabToken { host })
            }
            _ => Err(CredentialError::BadId(s.to_string())),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, CredentialError> {
        let wire = self.wire_id();
        match keyring::Entry::new(SERVICE, &wire) {
            Ok(entry) => Ok(entry),
            Err(e) if CredentialError::is_keychain_unavailable(&e) => {
                Err(CredentialError::KeychainUnavailable)
            }
            Err(keyring::Error::TooLong(_, _)) | Err(keyring::Error::Invalid(_, _)) => {
                Err(CredentialError::BadId(wire))
            }
            Err(_) => Err(CredentialError::StoreFailed),
        }
    }
}

fn parse_host(raw: &str) -> Result<String, CredentialError> {
    let h = raw.trim().to_ascii_lowercase();
    if h.is_empty() || h.contains('/') || h.contains(char::is_whitespace) {
        return Err(CredentialError::BadId(raw.to_string()));
    }
    Ok(h)
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CredentialError {
    #[error("unknown credential id: {0}")]
    BadId(String),
    #[error("credential value must not be empty")]
    EmptyValue,
    #[error("OS keychain is unavailable on this machine (no secret service); set env vars or run on a host with a keychain")]
    KeychainUnavailable,
    #[error("failed to store credential in the OS keychain")]
    StoreFailed,
}

impl CredentialError {
    pub fn is_keychain_unavailable(err: &keyring::Error) -> bool {
        matches!(
            err,
            keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_)
        )
    }
}

/// Remove a credential from the keychain (tests; not exposed on the API).
pub fn delete(id: &CredentialId) -> Result<(), CredentialError> {
    if mem_enabled() {
        MEM_STORE.lock().unwrap().remove(&id.wire_id());
        return Ok(());
    }
    let entry = id.entry()?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) if CredentialError::is_keychain_unavailable(&e) => {
            Err(CredentialError::KeychainUnavailable)
        }
        Err(_) => Err(CredentialError::StoreFailed),
    }
}

/// Store a secret in the OS keychain. Safe ids only appear in logs.
pub fn store(id: &CredentialId, value: &str) -> Result<(), CredentialError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CredentialError::EmptyValue);
    }
    if forced_unavailable() {
        return Err(CredentialError::KeychainUnavailable);
    }
    if mem_enabled() {
        MEM_STORE
            .lock()
            .unwrap()
            .insert(id.wire_id(), value.to_string());
        return Ok(());
    }
    let entry = id.entry()?;
    match entry.set_password(value) {
        Ok(()) => Ok(()),
        Err(e) if CredentialError::is_keychain_unavailable(&e) => {
            Err(CredentialError::KeychainUnavailable)
        }
        Err(_) => Err(CredentialError::StoreFailed),
    }
}

pub fn load(id: &CredentialId) -> Result<Option<String>, CredentialError> {
    if mem_enabled() {
        return Ok(MEM_STORE.lock().unwrap().get(&id.wire_id()).cloned());
    }
    let entry = id.entry()?;
    match entry.get_password() {
        Ok(v) => {
            let v = v.trim().to_string();
            Ok((!v.is_empty()).then_some(v))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) if CredentialError::is_keychain_unavailable(&e) => {
            Err(CredentialError::KeychainUnavailable)
        }
        Err(_) => Ok(None),
    }
}

/// True when a provider env var or keychain entry is set (never returns the value).
pub fn has_anthropic() -> bool {
    env_nonempty("ANTHROPIC_API_KEY")
        || env_nonempty("ANTHROPIC_AUTH_TOKEN")
        || keychain_present(&CredentialId::AnthropicApiKey)
        || keychain_present(&CredentialId::AnthropicAuthToken)
}

pub fn has_openai() -> bool {
    env_nonempty("OPENAI_API_KEY") || keychain_present(&CredentialId::OpenAiApiKey)
}

pub fn has_gemini() -> bool {
    env_nonempty("GEMINI_API_KEY") || keychain_present(&CredentialId::GeminiApiKey)
}

pub fn anthropic_api_key() -> Option<String> {
    env_or_keychain("ANTHROPIC_API_KEY", &CredentialId::AnthropicApiKey)
}

pub fn anthropic_auth_token() -> Option<String> {
    env_or_keychain("ANTHROPIC_AUTH_TOKEN", &CredentialId::AnthropicAuthToken)
}

pub fn openai_api_key() -> Option<String> {
    env_or_keychain("OPENAI_API_KEY", &CredentialId::OpenAiApiKey)
}

pub fn gemini_api_key() -> Option<String> {
    env_or_keychain("GEMINI_API_KEY", &CredentialId::GeminiApiKey)
}

pub fn github_token(host: &str) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    env_or_keychain_forge(&host, &CredentialId::GitHubToken { host: host.clone() })
}

pub fn gitlab_token(host: &str) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    env_or_keychain_forge(&host, &CredentialId::GitLabToken { host: host.clone() })
}

fn env_nonempty(var: &str) -> bool {
    std::env::var(var)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn env_or_keychain(var: &str, id: &CredentialId) -> Option<String> {
    if let Ok(v) = std::env::var(var) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    match load(id) {
        Ok(v) => v,
        Err(CredentialError::KeychainUnavailable) => None,
        Err(_) => None,
    }
}

fn env_or_keychain_forge(_host: &str, id: &CredentialId) -> Option<String> {
    // Env resolution stays in ferro-forge; this only reads the keychain slot.
    match load(id) {
        Ok(v) => v,
        Err(CredentialError::KeychainUnavailable) => None,
        Err(_) => None,
    }
}

fn keychain_present(id: &CredentialId) -> bool {
    matches!(load(id), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_ids_round_trip() {
        let ids = [
            CredentialId::AnthropicApiKey,
            CredentialId::GitHubToken {
                host: "github.com".into(),
            },
        ];
        for id in ids {
            let w = id.wire_id();
            assert_eq!(CredentialId::parse(&w).unwrap(), id);
        }
    }

    #[test]
    fn store_and_load_never_hits_state_dir() {
        enable_mem_store_for_tests();
        let id = CredentialId::OpenAiApiKey;
        store(&id, "sk-test-keychain-value-12345").unwrap();
        let v = load(&id).unwrap().unwrap();
        assert_eq!(v, "sk-test-keychain-value-12345");
    }
}

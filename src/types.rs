use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,
    Codex,
    Opencode,
}

impl ProviderKind {
    #[must_use]
    pub const fn canonical_id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_id())
    }
}

impl FromStr for ProviderKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::Opencode),
            other => Err(format!("unknown provider kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Anthropic,
    Codex,
    Opencode,
}

impl ProviderId {
    #[must_use]
    pub const fn storage_prefix(self) -> &'static str {
        match self {
            Self::Anthropic => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anthropic => formatter.write_str("anthropic"),
            Self::Codex => formatter.write_str("codex"),
            Self::Opencode => formatter.write_str("opencode"),
        }
    }
}

impl FromStr for ProviderId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::Opencode),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceCodes {
    pub code_verifier: String,
    pub code_challenge: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: String,
    pub email: String,
    pub expires_at: String,
    pub account_uuid: String,
    pub provider: ProviderId,
    pub id_token: Option<String>,
    pub last_refresh_at: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageData {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub reasoning_output_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableAccount {
    pub token: TokenData,
    pub device_id: String,
    pub account_uuid: String,
    pub provider: ProviderId,
    pub chatgpt_account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshTokenExhaustedError {
    pub reason: String,
    pub status_code: Option<u16>,
    pub body: Option<String>,
}

impl RefreshTokenExhaustedError {
    #[must_use]
    pub fn new(reason: impl Into<String>, status_code: Option<u16>, body: Option<String>) -> Self {
        Self {
            reason: reason.into(),
            status_code,
            body,
        }
    }
}

impl fmt::Display for RefreshTokenExhaustedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "refresh token {}", self.reason)
    }
}

impl std::error::Error for RefreshTokenExhaustedError {}

#[cfg(test)]
mod tests {
    use super::{ProviderId, ProviderKind};

    #[test]
    fn provider_id_parses_and_displays() {
        assert_eq!("opencode".parse::<ProviderId>(), Ok(ProviderId::Opencode));
        assert_eq!(ProviderId::Opencode.to_string(), "opencode");
        assert_eq!(ProviderId::Opencode.storage_prefix(), "opencode");
        assert!("nope".parse::<ProviderId>().is_err());
    }

    #[test]
    fn provider_kind_canonical_ids_are_kebab_case() {
        assert_eq!(ProviderKind::Anthropic.canonical_id(), "anthropic");
        assert_eq!(ProviderKind::Codex.canonical_id(), "codex");
        assert_eq!(ProviderKind::Opencode.canonical_id(), "opencode");
    }

    #[test]
    fn provider_kind_parses_from_str() {
        assert_eq!(
            "anthropic".parse::<ProviderKind>(),
            Ok(ProviderKind::Anthropic)
        );
        assert_eq!("codex".parse::<ProviderKind>(), Ok(ProviderKind::Codex));
        assert_eq!(
            "opencode".parse::<ProviderKind>(),
            Ok(ProviderKind::Opencode)
        );
        assert!("nope".parse::<ProviderKind>().is_err());
    }
}

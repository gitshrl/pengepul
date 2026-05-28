use std::path::Path;

use regex::Regex;

use crate::translate::resolve_model;
use crate::types::ProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub id: ProviderId,
    pub native_format: &'static str,
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: Vec<Provider>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn get(&self, provider_id: ProviderId) -> Provider {
        if let Some(provider) = self
            .providers
            .iter()
            .copied()
            .find(|provider| provider.id == provider_id)
        {
            return provider;
        }
        match provider_id {
            ProviderId::Anthropic => Provider {
                id: ProviderId::Anthropic,
                native_format: "anthropic-messages",
            },
            ProviderId::Codex => Provider {
                id: ProviderId::Codex,
                native_format: "openai-responses",
            },
        }
    }

    #[must_use]
    pub fn all(&self) -> &[Provider] {
        &self.providers
    }

    #[must_use]
    pub fn for_model(&self, model: &str) -> Provider {
        let resolved = resolve_model(Some(model));
        let codex = self.get(ProviderId::Codex);
        let anthropic = self.get(ProviderId::Anthropic);
        if codex_matches_model(&resolved) {
            return codex;
        }
        if anthropic_matches_model(&resolved) {
            return anthropic;
        }
        anthropic
    }
}

#[must_use]
pub fn build_registry(_auth_dir: &Path) -> ProviderRegistry {
    ProviderRegistry {
        providers: vec![
            Provider {
                id: ProviderId::Anthropic,
                native_format: "anthropic-messages",
            },
            Provider {
                id: ProviderId::Codex,
                native_format: "openai-responses",
            },
        ],
    }
}

fn codex_matches_model(model: &str) -> bool {
    Regex::new(r"(?i)^(gpt-5(\.|-)|gpt-5$|o\d|codex-)")
        .expect("valid codex model regex")
        .is_match(model)
}

fn anthropic_matches_model(model: &str) -> bool {
    Regex::new(r"(?i)^claude-")
        .expect("valid anthropic model regex")
        .is_match(model)
}

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
            ProviderId::OpenCodeGo => Provider {
                id: ProviderId::OpenCodeGo,
                native_format: "openai-chat",
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
        if opencode_go_matches_model(&resolved) {
            return self.get(ProviderId::OpenCodeGo);
        }
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

/// Prefix that routes a model to the opencode-go provider, e.g. `opencode-go/glm-5.1`.
pub const OPENCODE_GO_PREFIX: &str = "opencode-go/";

/// Model ids served by opencode-go (from the models.dev catalog), reported on `/v1/models`.
pub const OPENCODE_GO_MODELS: [&str; 15] = [
    "glm-5.1",
    "glm-5",
    "kimi-k2.6",
    "kimi-k2.5",
    "deepseek-v4-pro",
    "deepseek-v4-flash",
    "minimax-m2.7",
    "minimax-m2.5",
    "qwen3.7-max",
    "qwen3.6-plus",
    "qwen3.5-plus",
    "mimo-v2.5-pro",
    "mimo-v2.5",
    "mimo-v2-pro",
    "mimo-v2-omni",
];

/// Strip the `opencode-go/` routing prefix to get the upstream model id.
#[must_use]
pub fn strip_opencode_go_prefix(model: &str) -> &str {
    model.strip_prefix(OPENCODE_GO_PREFIX).unwrap_or(model)
}

fn opencode_go_matches_model(model: &str) -> bool {
    model.starts_with(OPENCODE_GO_PREFIX)
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
            Provider {
                id: ProviderId::OpenCodeGo,
                native_format: "openai-chat",
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

#[cfg(test)]
mod tests {
    use super::{build_registry, strip_opencode_go_prefix};
    use crate::types::ProviderId;
    use std::path::Path;

    #[test]
    fn routes_opencode_go_prefix() {
        let registry = build_registry(Path::new("/tmp"));
        assert_eq!(
            registry.for_model("opencode-go/glm-5.1").id,
            ProviderId::OpenCodeGo
        );
        // a bare opencode-go model id (no prefix) must NOT hijack other providers.
        assert_eq!(registry.for_model("glm-5.1").id, ProviderId::Anthropic);
        assert_eq!(
            registry.for_model("claude-sonnet-4-6").id,
            ProviderId::Anthropic
        );
        assert_eq!(registry.for_model("gpt-5.4").id, ProviderId::Codex);
    }

    #[test]
    fn strips_prefix_for_upstream() {
        assert_eq!(
            strip_opencode_go_prefix("opencode-go/kimi-k2.6"),
            "kimi-k2.6"
        );
        assert_eq!(strip_opencode_go_prefix("kimi-k2.6"), "kimi-k2.6");
    }
}

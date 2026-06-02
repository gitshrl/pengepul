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
            ProviderId::Opencode => Provider {
                id: ProviderId::Opencode,
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
        if opencode_matches_model(&resolved) {
            return self.get(ProviderId::Opencode);
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

/// Prefix that routes a model to the opencode provider, e.g. `opencode/glm-5.1`.
pub const OPENCODE_PREFIX: &str = "opencode/";

/// Model ids served by opencode (from the models.dev catalog), reported on `/v1/models`.
pub const OPENCODE_MODELS: [&str; 15] = [
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

/// Free-tier model ids served by opencode zen, reported on `/v1/models`. Unlike the paid
/// go-plan models these route to the credits endpoint (`/zen/v1`) rather than `/zen/go/v1`.
pub const OPENCODE_FREE_MODELS: [&str; 5] = [
    "deepseek-v4-flash-free",
    "mimo-v2.5-free",
    "qwen3.6-plus-free",
    "minimax-m3-free",
    "nemotron-3-super-free",
];

/// Strip the `opencode/` routing prefix to get the upstream model id.
#[must_use]
pub fn strip_opencode_prefix(model: &str) -> &str {
    model.strip_prefix(OPENCODE_PREFIX).unwrap_or(model)
}

/// True when `model` (with or without the `opencode/` prefix) is a free-tier zen model.
#[must_use]
pub fn is_opencode_free_model(model: &str) -> bool {
    OPENCODE_FREE_MODELS.contains(&strip_opencode_prefix(model))
}

fn opencode_matches_model(model: &str) -> bool {
    model.starts_with(OPENCODE_PREFIX)
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
                id: ProviderId::Opencode,
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
    use super::{build_registry, strip_opencode_prefix};
    use crate::types::ProviderId;
    use std::path::Path;

    #[test]
    fn routes_opencode_prefix() {
        let registry = build_registry(Path::new("/tmp"));
        assert_eq!(
            registry.for_model("opencode/glm-5.1").id,
            ProviderId::Opencode
        );
        assert_eq!(
            registry.for_model("opencode/deepseek-v4-flash-free").id,
            ProviderId::Opencode
        );
        // a bare opencode model id (no prefix) must NOT hijack other providers.
        assert_eq!(registry.for_model("glm-5.1").id, ProviderId::Anthropic);
        assert_eq!(
            registry.for_model("claude-sonnet-4-6").id,
            ProviderId::Anthropic
        );
        assert_eq!(registry.for_model("gpt-5.4").id, ProviderId::Codex);
    }

    #[test]
    fn strips_prefix_for_upstream() {
        assert_eq!(strip_opencode_prefix("opencode/kimi-k2.6"), "kimi-k2.6");
        assert_eq!(
            strip_opencode_prefix("opencode/deepseek-v4-flash-free"),
            "deepseek-v4-flash-free"
        );
        assert_eq!(strip_opencode_prefix("kimi-k2.6"), "kimi-k2.6");
    }

    #[test]
    fn classifies_free_models_with_or_without_prefix() {
        assert!(super::is_opencode_free_model("deepseek-v4-flash-free"));
        assert!(super::is_opencode_free_model(
            "opencode/nemotron-3-super-free"
        ));
        // the paid twin of a free model is not free.
        assert!(!super::is_opencode_free_model("deepseek-v4-flash"));
        assert!(!super::is_opencode_free_model("opencode/glm-5.1"));
    }
}

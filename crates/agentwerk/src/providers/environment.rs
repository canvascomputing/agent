//! Resolves an LLM provider, model name, and runtime overrides from environment
//! variables, so a caller writes neither the detection order nor the overrides.

use super::error::{ProviderError, ProviderResult};
use super::{Anthropic, LiteLlm, Mistral, OpenAi, Provider};

/// Detected provider name, before constructing the actual provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetectedProvider {
    Anthropic,
    Mistral,
    OpenAi,
    LiteLlm,
}

/// Read an environment variable, falling back to `default`. An empty value
/// counts as unset.
pub(crate) fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into())
}

/// Read an environment variable a provider cannot be built without. An empty
/// value counts as unset and fails the same way.
pub(crate) fn env_required(name: &'static str) -> ProviderResult<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ProviderError::ProviderUnrecognized {
            message: format!("{name} environment variable not set"),
        })
}

/// Read an environment variable that may be absent. An empty value counts as
/// unset.
pub(crate) fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Detect an LLM provider from environment variables and construct it from
/// API key and base URL. It picks no model: pair it with [`model_from_env`], or
/// set one on the agent.
///
/// Detection order:
///   0. `LITELLM_PROVIDER` → explicit selection (`anthropic`, `mistral`, `openai`, `litellm`)
///   1. `LITELLM_API_KEY`  → LiteLLM proxy (URL from `LITELLM_BASE_URL`, default `http://localhost:4000`)
///   2. `MISTRAL_API_KEY`  → Mistral
///   3. `ANTHROPIC_API_KEY` → Anthropic
///   4. `OPENAI_API_KEY`   → OpenAI
///
/// Empty env vars are treated as unset.
pub(crate) fn provider_from_env() -> ProviderResult<Provider> {
    let detected = detect_provider_name(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))?;
    Ok(match detected {
        DetectedProvider::Anthropic => Provider::new(Anthropic::from_env()?),
        DetectedProvider::Mistral => Provider::new(Mistral::from_env()?),
        DetectedProvider::OpenAi => Provider::new(OpenAi::from_env()?),
        DetectedProvider::LiteLlm => Provider::new(LiteLlm::from_env()?),
    })
}

/// Resolve a model name from environment variables.
///
/// Priority:
///   1. `MODEL`: a generic override that wins whatever the LLM provider is.
///   2. `*_MODEL`: named after the provider, chosen the same way
///      [`provider_from_env`] chooses one, as in `OPENAI_MODEL`.
///   3. The vendor's own default model for the detected provider.
pub(crate) fn model_from_env() -> ProviderResult<String> {
    model_from_env_with(|name| std::env::var(name).ok())
}

pub(crate) fn model_from_env_with<F>(get: F) -> ProviderResult<String>
where
    F: Fn(&str) -> Option<String>,
{
    let filtered = |name: &str| get(name).filter(|v| !v.is_empty());

    if let Some(m) = filtered("MODEL") {
        return Ok(m);
    }

    let detected = detect_provider_name(filtered)?;
    let (model_var, default_model) = match detected {
        DetectedProvider::Anthropic => ("ANTHROPIC_MODEL", "claude-sonnet-4-20250514"),
        DetectedProvider::Mistral => ("MISTRAL_MODEL", "mistral-medium-2508"),
        DetectedProvider::OpenAi => ("OPENAI_MODEL", "gpt-4o"),
        DetectedProvider::LiteLlm => ("LITELLM_MODEL", "claude-sonnet-4-20250514"),
    };
    Ok(filtered(model_var).unwrap_or_else(|| default_model.to_string()))
}

/// Caller-supplied override for the model's context window, read from
/// `MODEL_CONTEXT_WINDOW`. Use when the registry guess (see
/// [`super::Model::new`]) disagrees with the runtime's actual
/// window: a local llama.cpp or vLLM deployment whose name does not
/// match any registry entry, or a hosted model whose deployment was
/// truncated below its native window. Returns `None` when the variable
/// is unset, empty, or not a positive integer.
pub(crate) fn context_window_from_env() -> Option<u64> {
    context_window_from_env_with(|name| std::env::var(name).ok())
}

pub(crate) fn context_window_from_env_with<F>(get: F) -> Option<u64>
where
    F: Fn(&str) -> Option<String>,
{
    get("MODEL_CONTEXT_WINDOW")
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
}

/// Decide which provider the environment names, reading it through `get_env` so
/// the order is testable without setting variables.
pub(crate) fn detect_provider_name<F>(get_env: F) -> ProviderResult<DetectedProvider>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(name) = get_env("LITELLM_PROVIDER") {
        return match name.as_str() {
            "anthropic" => Ok(DetectedProvider::Anthropic),
            "mistral" => Ok(DetectedProvider::Mistral),
            "openai" => Ok(DetectedProvider::OpenAi),
            "litellm" => Ok(DetectedProvider::LiteLlm),
            other => Err(ProviderError::ProviderUnrecognized {
                message: format!(
                    "Unknown LITELLM_PROVIDER \"{other}\". Supported: anthropic, mistral, openai, litellm"
                ),
            }),
        };
    }

    if get_env("LITELLM_API_KEY").is_some() {
        return Ok(DetectedProvider::LiteLlm);
    }

    if get_env("MISTRAL_API_KEY").is_some() {
        return Ok(DetectedProvider::Mistral);
    }

    if get_env("ANTHROPIC_API_KEY").is_some() {
        return Ok(DetectedProvider::Anthropic);
    }

    if get_env("OPENAI_API_KEY").is_some() {
        return Ok(DetectedProvider::OpenAi);
    }

    Err(ProviderError::ProviderUnrecognized {
        message: "No LLM provider found. Set one of: LITELLM_PROVIDER, LITELLM_API_KEY, MISTRAL_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY"
            .into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
                .filter(|v| !v.is_empty())
        }
    }

    #[test]
    fn an_explicit_name_selects_anthropic() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "anthropic"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Anthropic);
    }

    #[test]
    fn an_explicit_name_selects_mistral() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "mistral"),
            ("MISTRAL_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Mistral);
    }

    #[test]
    fn an_explicit_name_selects_openai() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "openai"),
            ("OPENAI_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::OpenAi);
    }

    #[test]
    fn an_explicit_name_selects_the_litellm_proxy() {
        let result = detect_provider_name(env_map(&[("LITELLM_PROVIDER", "litellm")])).unwrap();
        assert_eq!(result, DetectedProvider::LiteLlm);
    }

    #[test]
    fn an_explicit_name_outranks_every_key_that_is_set() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "anthropic"),
            ("ANTHROPIC_API_KEY", "key"),
            ("OPENAI_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Anthropic);
    }

    #[test]
    fn a_litellm_key_alone_selects_the_proxy() {
        let result = detect_provider_name(env_map(&[("LITELLM_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::LiteLlm);
    }

    #[test]
    fn a_mistral_key_alone_selects_mistral() {
        let result = detect_provider_name(env_map(&[("MISTRAL_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::Mistral);
    }

    #[test]
    fn an_anthropic_key_alone_selects_anthropic() {
        let result = detect_provider_name(env_map(&[("ANTHROPIC_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::Anthropic);
    }

    #[test]
    fn an_openai_key_alone_selects_openai() {
        let result = detect_provider_name(env_map(&[("OPENAI_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::OpenAi);
    }

    #[test]
    fn a_litellm_key_outranks_the_vendor_keys_beside_it() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_API_KEY", "key"),
            ("MISTRAL_API_KEY", "key"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::LiteLlm);
    }

    #[test]
    fn a_mistral_key_outranks_an_anthropic_one() {
        let result = detect_provider_name(env_map(&[
            ("MISTRAL_API_KEY", "key"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Mistral);
    }

    #[test]
    fn a_name_no_provider_answers_to_is_refused() {
        let err = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "invalid"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("Unknown LITELLM_PROVIDER"));
    }

    #[test]
    fn an_environment_naming_no_provider_is_refused() {
        let err = detect_provider_name(env_map(&[])).unwrap_err();
        assert!(err.to_string().contains("No LLM provider found"));
    }

    #[test]
    fn an_empty_key_counts_as_unset() {
        let err = detect_provider_name(env_map(&[("ANTHROPIC_API_KEY", "")])).unwrap_err();
        assert!(err.to_string().contains("No LLM provider found"));
    }

    #[test]
    fn a_bare_model_name_outranks_the_provider_prefixed_one() {
        let model = model_from_env_with(env_map(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_MODEL", "gpt-4-turbo"),
            ("MODEL", "override"),
        ]))
        .unwrap();
        assert_eq!(model, "override");
    }

    #[test]
    fn the_provider_prefixed_model_name_is_read_when_no_bare_one_is_set() {
        let model = model_from_env_with(env_map(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_MODEL", "gpt-4-turbo"),
        ]))
        .unwrap();
        assert_eq!(model, "gpt-4-turbo");
    }

    #[test]
    fn a_provider_with_no_model_named_takes_its_own_default() {
        let model = model_from_env_with(env_map(&[("OPENAI_API_KEY", "key")])).unwrap();
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn every_provider_carries_its_own_default_model() {
        let anthropic = model_from_env_with(env_map(&[("ANTHROPIC_API_KEY", "k")])).unwrap();
        assert_eq!(anthropic, "claude-sonnet-4-20250514");

        let mistral = model_from_env_with(env_map(&[("MISTRAL_API_KEY", "k")])).unwrap();
        assert_eq!(mistral, "mistral-medium-2508");

        let litellm = model_from_env_with(env_map(&[("LITELLM_API_KEY", "k")])).unwrap();
        assert_eq!(litellm, "claude-sonnet-4-20250514");
    }

    #[test]
    fn a_model_cannot_be_named_when_no_provider_is() {
        let err = model_from_env_with(env_map(&[])).unwrap_err();
        assert!(err.to_string().contains("No LLM provider found"));
    }

    #[test]
    fn an_empty_provider_prefixed_model_name_falls_to_the_default() {
        let model =
            model_from_env_with(env_map(&[("OPENAI_API_KEY", "key"), ("OPENAI_MODEL", "")]))
                .unwrap();
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn a_context_window_override_is_read_as_a_number() {
        let n = context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "65536")]));
        assert_eq!(n, Some(65_536));
    }

    #[test]
    fn an_unset_context_window_leaves_the_registry_to_answer() {
        assert_eq!(context_window_from_env_with(env_map(&[])), None);
    }

    #[test]
    fn an_empty_context_window_leaves_the_registry_to_answer() {
        assert_eq!(
            context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "")])),
            None,
        );
    }

    #[test]
    fn a_context_window_that_is_not_a_number_leaves_the_registry_to_answer() {
        assert_eq!(
            context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "huge")])),
            None,
        );
    }

    #[test]
    fn a_zero_context_window_leaves_the_registry_to_answer() {
        // Zero is a parse success but a meaningless window; treat as unset
        // so the registry guess still gets a chance.
        assert_eq!(
            context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "0")])),
            None,
        );
    }
}

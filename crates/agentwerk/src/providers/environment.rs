//! Resolves an LLM provider, model name, and runtime overrides from environment variables so callers do not have to code the detection matrix or the override knobs themselves.
//!
//! [`load_dot_env`] is the one reader that also writes: call it at startup,
//! before threads that read the environment are spawned.

use super::error::{ProviderError, ProviderResult};
use super::{AnthropicProvider, LiteLlmProvider, MistralProvider, OpenAiProvider, Provider};

const DOT_ENV: &str = ".env";

/// Detected provider name, before constructing the actual provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetectedProvider {
    Anthropic,
    Mistral,
    OpenAi,
    LiteLlm,
}

/// Read an env var, treating empty values as unset. Returns `default` if missing or empty.
pub(crate) fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into())
}

/// Read a required env var. Returns `Err` if missing or empty.
pub(crate) fn env_required(name: &'static str) -> ProviderResult<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ProviderError::ProviderUnrecognized {
            message: format!("{name} environment variable not set"),
        })
}

/// Read an optional env var, treating empty values as unset. Returns `None` if missing or empty.
pub(crate) fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Writes the process environment rather than threading the values through, so
/// `SSL_CERT_FILE` and host-side reads see the file too. A missing or malformed
/// file is an error: a caller who names the file means it.
pub(crate) fn load_dot_env() -> ProviderResult<()> {
    let contents =
        std::fs::read_to_string(DOT_ENV).map_err(|e| ProviderError::ProviderUnrecognized {
            message: format!("{DOT_ENV} could not be read from the current directory: {e}"),
        })?;
    for (name, value) in unset_pairs(parse_dot_env(&contents)?, |name| env_opt(name)) {
        std::env::set_var(name, value);
    }
    Ok(())
}

/// Parse `.env` contents into name and value pairs, in file order. `export ` is
/// accepted so the same file still sources in a shell. Nothing is expanded and
/// no escape sequence is honoured: every value is literal.
fn parse_dot_env(contents: &str) -> ProviderResult<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    for (index, raw) in contents.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let statement = line.strip_prefix("export ").unwrap_or(line);
        let (name, value) = statement
            .split_once('=')
            .ok_or_else(|| malformed(index, line))?;
        let name = name.trim();
        if name.is_empty() {
            return Err(malformed(index, line));
        }
        pairs.push((name.to_string(), unquote(value.trim())));
    }
    Ok(pairs)
}

/// Where precedence is decided: `.env` fills gaps, never overrides.
fn unset_pairs<F>(pairs: Vec<(String, String)>, get: F) -> Vec<(String, String)>
where
    F: Fn(&str) -> Option<String>,
{
    pairs
        .into_iter()
        .filter(|(name, _)| get(name).is_none())
        .collect()
}

/// The comment cut needs the leading space: a bare `#` is legal inside a key.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    match value.split_once(" #") {
        Some((before, _)) => before.trim_end().to_string(),
        None => value.to_string(),
    }
}

fn malformed(index: usize, line: &str) -> ProviderError {
    ProviderError::ProviderUnrecognized {
        message: format!(
            "{DOT_ENV} line {}: expected NAME=VALUE, found \"{line}\"",
            index + 1
        ),
    }
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
        DetectedProvider::Anthropic => Provider::new(AnthropicProvider::from_env()?),
        DetectedProvider::Mistral => Provider::new(MistralProvider::from_env()?),
        DetectedProvider::OpenAi => Provider::new(OpenAiProvider::from_env()?),
        DetectedProvider::LiteLlm => Provider::new(LiteLlmProvider::from_env()?),
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
/// [`super::Model::from_name`]) disagrees with the runtime's actual
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

/// Pure detection logic. Determines which provider to use based on env var values.
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
    fn explicit_anthropic() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "anthropic"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Anthropic);
    }

    #[test]
    fn explicit_mistral() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "mistral"),
            ("MISTRAL_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Mistral);
    }

    #[test]
    fn explicit_openai() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "openai"),
            ("OPENAI_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::OpenAi);
    }

    #[test]
    fn explicit_litellm() {
        let result = detect_provider_name(env_map(&[("LITELLM_PROVIDER", "litellm")])).unwrap();
        assert_eq!(result, DetectedProvider::LiteLlm);
    }

    #[test]
    fn explicit_overrides_auto_detection() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "anthropic"),
            ("ANTHROPIC_API_KEY", "key"),
            ("OPENAI_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Anthropic);
    }

    #[test]
    fn auto_litellm_api_key() {
        let result = detect_provider_name(env_map(&[("LITELLM_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::LiteLlm);
    }

    #[test]
    fn auto_mistral() {
        let result = detect_provider_name(env_map(&[("MISTRAL_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::Mistral);
    }

    #[test]
    fn auto_anthropic() {
        let result = detect_provider_name(env_map(&[("ANTHROPIC_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::Anthropic);
    }

    #[test]
    fn auto_openai() {
        let result = detect_provider_name(env_map(&[("OPENAI_API_KEY", "key")])).unwrap();
        assert_eq!(result, DetectedProvider::OpenAi);
    }

    #[test]
    fn litellm_key_wins_over_others() {
        let result = detect_provider_name(env_map(&[
            ("LITELLM_API_KEY", "key"),
            ("MISTRAL_API_KEY", "key"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::LiteLlm);
    }

    #[test]
    fn mistral_wins_over_anthropic() {
        let result = detect_provider_name(env_map(&[
            ("MISTRAL_API_KEY", "key"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap();
        assert_eq!(result, DetectedProvider::Mistral);
    }

    #[test]
    fn invalid_provider_returns_error() {
        let err = detect_provider_name(env_map(&[
            ("LITELLM_PROVIDER", "invalid"),
            ("ANTHROPIC_API_KEY", "key"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("Unknown LITELLM_PROVIDER"));
    }

    #[test]
    fn no_provider_returns_error() {
        let err = detect_provider_name(env_map(&[])).unwrap_err();
        assert!(err.to_string().contains("No LLM provider found"));
    }

    #[test]
    fn empty_values_treated_as_unset() {
        let err = detect_provider_name(env_map(&[("ANTHROPIC_API_KEY", "")])).unwrap_err();
        assert!(err.to_string().contains("No LLM provider found"));
    }

    #[test]
    fn model_generic_wins_over_provider_prefixed() {
        let model = model_from_env_with(env_map(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_MODEL", "gpt-4-turbo"),
            ("MODEL", "override"),
        ]))
        .unwrap();
        assert_eq!(model, "override");
    }

    #[test]
    fn model_provider_prefixed_used_when_generic_unset() {
        let model = model_from_env_with(env_map(&[
            ("OPENAI_API_KEY", "key"),
            ("OPENAI_MODEL", "gpt-4-turbo"),
        ]))
        .unwrap();
        assert_eq!(model, "gpt-4-turbo");
    }

    #[test]
    fn model_falls_back_to_hosted_default() {
        let model = model_from_env_with(env_map(&[("OPENAI_API_KEY", "key")])).unwrap();
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn model_hosted_defaults_per_provider() {
        let anthropic = model_from_env_with(env_map(&[("ANTHROPIC_API_KEY", "k")])).unwrap();
        assert_eq!(anthropic, "claude-sonnet-4-20250514");

        let mistral = model_from_env_with(env_map(&[("MISTRAL_API_KEY", "k")])).unwrap();
        assert_eq!(mistral, "mistral-medium-2508");

        let litellm = model_from_env_with(env_map(&[("LITELLM_API_KEY", "k")])).unwrap();
        assert_eq!(litellm, "claude-sonnet-4-20250514");
    }

    #[test]
    fn model_errors_when_no_provider_detected() {
        let err = model_from_env_with(env_map(&[])).unwrap_err();
        assert!(err.to_string().contains("No LLM provider found"));
    }

    #[test]
    fn model_empty_provider_prefixed_falls_through_to_default() {
        let model =
            model_from_env_with(env_map(&[("OPENAI_API_KEY", "key"), ("OPENAI_MODEL", "")]))
                .unwrap();
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn context_window_returns_parsed_value() {
        let n = context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "65536")]));
        assert_eq!(n, Some(65_536));
    }

    #[test]
    fn context_window_unset_returns_none() {
        assert_eq!(context_window_from_env_with(env_map(&[])), None);
    }

    #[test]
    fn context_window_empty_returns_none() {
        assert_eq!(
            context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "")])),
            None,
        );
    }

    #[test]
    fn context_window_garbage_returns_none() {
        assert_eq!(
            context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "huge")])),
            None,
        );
    }

    #[test]
    fn context_window_zero_returns_none() {
        // Zero is a parse success but a meaningless window; treat as unset
        // so the registry guess still gets a chance.
        assert_eq!(
            context_window_from_env_with(env_map(&[("MODEL_CONTEXT_WINDOW", "0")])),
            None,
        );
    }

    #[test]
    fn dot_env_reads_plain_and_exported_assignments() {
        let pairs = parse_dot_env("OPENAI_API_KEY=sk-local\nexport MODEL=gpt-4o\n").unwrap();
        assert_eq!(
            pairs,
            vec![
                ("OPENAI_API_KEY".to_string(), "sk-local".to_string()),
                ("MODEL".to_string(), "gpt-4o".to_string()),
            ],
        );
    }

    #[test]
    fn dot_env_skips_comments_and_blank_lines() {
        let pairs = parse_dot_env("# a note\n\n   \nMODEL=gpt-4o\n").unwrap();
        assert_eq!(pairs, vec![("MODEL".to_string(), "gpt-4o".to_string())]);
    }

    #[test]
    fn dot_env_unwraps_quoted_values() {
        let pairs = parse_dot_env("A=\"one two\"\nB='three four'\n").unwrap();
        assert_eq!(
            pairs,
            vec![
                ("A".to_string(), "one two".to_string()),
                ("B".to_string(), "three four".to_string()),
            ],
        );
    }

    #[test]
    fn dot_env_cuts_a_trailing_comment_off_an_unquoted_value() {
        let pairs = parse_dot_env("MODEL=gpt-4o # the cheap one\n").unwrap();
        assert_eq!(pairs, vec![("MODEL".to_string(), "gpt-4o".to_string())]);
    }

    #[test]
    fn dot_env_keeps_a_hash_inside_a_quoted_value() {
        let pairs = parse_dot_env("KEY=\"sk-a#b\"\n").unwrap();
        assert_eq!(pairs, vec![("KEY".to_string(), "sk-a#b".to_string())]);
    }

    #[test]
    fn dot_env_trims_whitespace_around_the_assignment() {
        let pairs = parse_dot_env("  MODEL  =  gpt-4o  \n").unwrap();
        assert_eq!(pairs, vec![("MODEL".to_string(), "gpt-4o".to_string())]);
    }

    #[test]
    fn dot_env_keeps_both_spellings_of_a_repeated_name() {
        // Deduplication is the writer's job: it applies in order, so the last wins.
        let pairs = parse_dot_env("MODEL=first\nMODEL=second\n").unwrap();
        assert_eq!(pairs.last().unwrap().1, "second");
    }

    #[test]
    fn dot_env_line_without_assignment_is_rejected() {
        let err = parse_dot_env("MODEL=gpt-4o\njust some words\n").unwrap_err();
        assert!(err.to_string().contains(".env line 2"));
        assert!(err.to_string().contains("just some words"));
    }

    #[test]
    fn dot_env_line_without_a_name_is_rejected() {
        let err = parse_dot_env("=gpt-4o\n").unwrap_err();
        assert!(err.to_string().contains(".env line 1"));
    }

    #[test]
    fn dot_env_yields_only_what_the_environment_lacks() {
        let pairs = vec![
            ("MODEL".to_string(), "from-file".to_string()),
            ("OPENAI_API_KEY".to_string(), "from-file".to_string()),
        ];
        let unset = unset_pairs(pairs, env_map(&[("MODEL", "from-shell")]));
        assert_eq!(
            unset,
            vec![("OPENAI_API_KEY".to_string(), "from-file".to_string())],
        );
    }

    #[test]
    fn dot_env_fills_a_name_the_environment_carries_empty() {
        let pairs = vec![("MODEL".to_string(), "from-file".to_string())];
        let unset = unset_pairs(pairs, env_map(&[("MODEL", "")]));
        assert_eq!(unset, vec![("MODEL".to_string(), "from-file".to_string())]);
    }
}

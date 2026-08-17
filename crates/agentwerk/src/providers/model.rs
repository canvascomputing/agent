//! What agentwerk knows about each model's context window, and when a
//! conversation has to be summarized to fit.

use super::ReasoningEffort;

/// Model metadata: the name plus anything we know about its capabilities.
///
/// Built by [`Model::from_name`] (registry-backed) or
/// [`Model::context_window`] (explicit override). The agent loop reads
/// `Model::context_window` to decide when a conversation needs to be
/// shrunk before the next request.
#[derive(Debug, Clone)]
pub struct Model {
    pub name: String,
    context_window: Option<u64>,
    reasoning_effort: ReasoningEffort,
}

impl Model {
    /// Build a `Model`, looking its context window up by name. An unknown name
    /// leaves `context_window` at `None`, so nothing is compacted and nothing
    /// fails.
    pub fn from_name(name: impl Into<String>) -> Self {
        let name = name.into();
        let context_window = context_window_for(&name);
        Self {
            name,
            context_window,
            reasoning_effort: ReasoningEffort::Off,
        }
    }

    /// Build a `Model` from the environment: the name, plus the context window
    /// when `MODEL_CONTEXT_WINDOW` is set.
    pub fn from_env() -> super::ProviderResult<Self> {
        let model = Self::from_name(super::environment::model_from_env()?);
        Ok(match super::environment::context_window_from_env() {
            Some(size) => model.context_window(size),
            None => model,
        })
    }

    /// Set the context window size for a model, skipping the known names. Useful for local proxies or
    /// private deployments whose name isn't in any provider's table.
    pub fn context_window(mut self, size: u64) -> Self {
        self.context_window = Some(size);
        self
    }

    /// Ask this model for extended thinking at the given depth. Off by
    /// default. See [`ReasoningEffort`](super::ReasoningEffort).
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Known context window size, `None` when the name is in no registry
    /// and no override was set.
    pub fn get_context_window(&self) -> Option<u64> {
        self.context_window
    }

    /// Requested extended-thinking depth for this model.
    pub fn get_reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }
}

impl From<&str> for Model {
    fn from(name: &str) -> Self {
        Self::from_name(name)
    }
}

impl From<String> for Model {
    fn from(name: String) -> Self {
        Self::from_name(name)
    }
}

impl From<&String> for Model {
    fn from(name: &String) -> Self {
        Self::from_name(name.as_str())
    }
}

impl From<&Model> for Model {
    fn from(model: &Model) -> Self {
        model.clone()
    }
}

/// The context window a model name is known to have, in tokens. One table
/// rather than one per LLM provider: the same name reaches agentwerk through
/// whichever endpoint serves it, and several of these families are served by
/// more than one.
fn context_window_for(name: &str) -> Option<u64> {
    let model = name.to_ascii_lowercase();

    if model.contains("[1m]") {
        return Some(1_000_000);
    }
    // The Claude 5 family is natively 1 000 000 tokens, with no [1m] opt-in.
    if model.contains("claude-fable-5")
        || model.contains("claude-mythos-5")
        || model.contains("claude-opus-5")
        || model.contains("claude-sonnet-5")
    {
        return Some(1_000_000);
    }
    if model.contains("claude-opus-4")
        || model.contains("claude-sonnet-4")
        || model.contains("claude-haiku-4")
        || model.contains("claude-3-7-sonnet")
        || model.contains("claude-3-5-sonnet")
        || model.contains("claude-3-5-haiku")
        || model.contains("claude-3-opus")
        || model.contains("claude-3-sonnet")
        || model.contains("claude-3-haiku")
    {
        return Some(200_000);
    }

    // Newest first so "gpt-4" doesn't shadow "gpt-4.1".
    if model.contains("gpt-4.1") {
        return Some(1_000_000);
    }
    if model.contains("gpt-5") {
        // 400K covers mini/nano; full/pro reach ~1M but we pick the family floor.
        return Some(400_000);
    }
    if model.starts_with("o3") || model.starts_with("o1") {
        return Some(200_000);
    }
    if model.contains("gpt-4o") || model.contains("gpt-4-turbo") {
        return Some(128_000);
    }
    if model.contains("gpt-4-32k") {
        return Some(32_768);
    }
    if model.contains("gpt-4") {
        return Some(8_192);
    }
    if model.contains("gpt-3.5-turbo") {
        return Some(16_385);
    }

    // Qwen values are the published *native* windows; deployments that disable
    // YaRN or configure a shorter `max_model_len` should override via
    // `Agent::model(Model::from_name(name).context_window(n))`.
    // Newest first so "qwen3" doesn't shadow "qwen3.5" / "qwen3.6".
    let qwen3_256k = model.contains("qwen3.6")
        || model.contains("qwen3.5")
        || model.contains("qwen3-coder")
        || model.contains("qwen3-next")
        || (model.contains("qwen3") && model.contains("-2507"));
    if qwen3_256k {
        return Some(262_144);
    }
    if model.contains("qwen3") {
        return Some(131_072);
    }
    if model.contains("qwen2.5") {
        // Qwen2.5 "1M" variants (e.g. Qwen2.5-7B-Instruct-1M) are a separate
        // release with a 1 000 000-token native window.
        if model.contains("-1m") {
            return Some(1_000_000);
        }
        return Some(32_768);
    }

    if model.contains("codestral") {
        return Some(256_000);
    }
    if model.contains("mistral-large")
        || model.contains("mistral-medium")
        || model.contains("mistral-small")
    {
        return Some(131_072);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_resolves_claude_models() {
        assert_eq!(
            Model::from_name("claude-sonnet-4-20250514").context_window,
            Some(200_000)
        );
    }

    #[test]
    fn from_name_resolves_openai_models() {
        assert_eq!(Model::from_name("gpt-5").context_window, Some(400_000));
        assert_eq!(Model::from_name("gpt-4o").context_window, Some(128_000));
    }

    #[test]
    fn from_name_resolves_mistral_models() {
        assert_eq!(
            Model::from_name("mistral-large-2411").context_window,
            Some(131_072)
        );
    }

    #[test]
    fn from_name_unknown_has_no_context_window() {
        assert_eq!(Model::from_name("unknown").context_window, None);
        assert_eq!(Model::from_name("mock").context_window, None);
    }

    #[test]
    fn context_window_overrides() {
        let m = Model::from_name("unknown").context_window(50_000);
        assert_eq!(m.context_window, Some(50_000));
    }

    /// The only test that writes the process environment, so nothing races it.
    #[test]
    fn from_env_reads_the_name_and_prefers_the_window_override() {
        std::env::set_var("ANTHROPIC_API_KEY", "key");
        std::env::set_var("MODEL", "claude-sonnet-4-20250514");
        std::env::set_var("MODEL_CONTEXT_WINDOW", "64000");

        let m = Model::from_env().unwrap();
        assert_eq!(m.name, "claude-sonnet-4-20250514");
        assert_eq!(m.context_window, Some(64_000));

        std::env::remove_var("MODEL_CONTEXT_WINDOW");
        assert_eq!(Model::from_env().unwrap().context_window, Some(200_000));

        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("MODEL");
    }

    #[test]
    fn lookup_claude_4_family_returns_200k() {
        assert_eq!(
            context_window_for("claude-sonnet-4-20250514"),
            Some(200_000)
        );
        assert_eq!(context_window_for("claude-opus-4-20250101"), Some(200_000));
        assert_eq!(
            context_window_for("claude-haiku-4-5-20251001"),
            Some(200_000)
        );
    }

    #[test]
    fn lookup_claude_5_family_returns_one_million() {
        assert_eq!(context_window_for("claude-fable-5"), Some(1_000_000));
        assert_eq!(context_window_for("claude-opus-5"), Some(1_000_000));
        assert_eq!(context_window_for("claude-sonnet-5"), Some(1_000_000));
        assert_eq!(context_window_for("claude-mythos-5"), Some(1_000_000));
    }

    #[test]
    fn lookup_claude_3_family_returns_200k() {
        assert_eq!(
            context_window_for("claude-3-5-sonnet-20241022"),
            Some(200_000)
        );
        assert_eq!(context_window_for("claude-3-opus-20240229"), Some(200_000));
    }

    #[test]
    fn lookup_one_million_suffix_overrides_base_family() {
        assert_eq!(
            context_window_for("claude-opus-4-7[1m]"),
            Some(1_000_000),
            "explicit [1m] opt-in promotes the whole family"
        );
        assert_eq!(
            context_window_for("claude-sonnet-4-20250514[1m]"),
            Some(1_000_000)
        );
    }

    #[test]
    fn lookup_gpt_5_family_returns_400k() {
        assert_eq!(context_window_for("gpt-5"), Some(400_000));
        assert_eq!(context_window_for("gpt-5-mini"), Some(400_000));
    }

    #[test]
    fn lookup_gpt_4_1_returns_one_million() {
        assert_eq!(context_window_for("gpt-4.1"), Some(1_000_000));
    }

    #[test]
    fn lookup_o_series_returns_200k() {
        assert_eq!(context_window_for("o3-mini"), Some(200_000));
        assert_eq!(context_window_for("o1-preview"), Some(200_000));
    }

    #[test]
    fn lookup_gpt_4o_and_turbo_return_128k() {
        assert_eq!(context_window_for("gpt-4o"), Some(128_000));
        assert_eq!(context_window_for("gpt-4o-mini"), Some(128_000));
        assert_eq!(context_window_for("gpt-4-turbo-2024-04-09"), Some(128_000));
    }

    #[test]
    fn lookup_legacy_gpt_4_returns_8k() {
        assert_eq!(context_window_for("gpt-4"), Some(8_192));
        assert_eq!(context_window_for("gpt-4-32k"), Some(32_768));
    }

    #[test]
    fn lookup_gpt_3_5_turbo_returns_16k() {
        assert_eq!(context_window_for("gpt-3.5-turbo"), Some(16_385));
        assert_eq!(context_window_for("gpt-3.5-turbo-16k"), Some(16_385));
    }

    #[test]
    fn lookup_qwen3_coder_returns_256k() {
        assert_eq!(context_window_for("qwen3-coder-next"), Some(262_144));
        assert_eq!(context_window_for("qwen3-30b"), Some(131_072));
    }

    #[test]
    fn lookup_mistral_families_return_their_own_windows() {
        assert_eq!(context_window_for("codestral-2501"), Some(256_000));
        assert_eq!(context_window_for("mistral-large-2411"), Some(131_072));
        assert_eq!(context_window_for("mistral-small-2409"), Some(131_072));
    }

    #[test]
    fn lookup_unknown_models_return_none() {
        assert_eq!(context_window_for("llama-3-70b"), None);
        assert_eq!(context_window_for("some-future-model"), None);
    }

    #[test]
    fn reasoning_effort_defaults_off_and_is_settable() {
        assert_eq!(
            Model::from_name("gpt-5").get_reasoning_effort(),
            ReasoningEffort::Off
        );
        let m = Model::from_name("gpt-5").reasoning_effort(ReasoningEffort::High);
        assert_eq!(m.get_reasoning_effort(), ReasoningEffort::High);
    }
}

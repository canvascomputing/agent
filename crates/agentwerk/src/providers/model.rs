//! What agentwerk knows about each model's context window, and when a
//! conversation has to be summarized to fit.

use super::{AnthropicProvider, MistralProvider, OpenAiProvider, ReasoningEffort};

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
    /// Build a `Model` by asking each provider in turn for its known context
    /// window. An unknown name leaves `context_window` at `None`, so nothing is
    /// compacted and nothing fails.
    pub fn from_name(name: impl Into<String>) -> Self {
        let name = name.into();
        let context_window = AnthropicProvider::lookup_context_window_size(&name)
            .or_else(|| OpenAiProvider::lookup_context_window_size(&name))
            .or_else(|| MistralProvider::lookup_context_window_size(&name));
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

    /// Fill the environment from a `.env` file in the current directory, then
    /// build a `Model` as [`from_env`](Self::from_env) does. An exported value
    /// wins over the file; a missing or malformed file is an `Err`.
    pub fn from_dot_env() -> super::ProviderResult<Self> {
        super::environment::load_dot_env()?;
        Self::from_env()
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
    fn reasoning_effort_defaults_off_and_is_settable() {
        assert_eq!(
            Model::from_name("gpt-5").get_reasoning_effort(),
            ReasoningEffort::Off
        );
        let m = Model::from_name("gpt-5").reasoning_effort(ReasoningEffort::High);
        assert_eq!(m.get_reasoning_effort(), ReasoningEffort::High);
    }
}

//! Connects agents to Anthropic, OpenAI-compatible APIs, Mistral, and LiteLLM.
//!
//! Request and response types remain available for custom [`ProviderLike`] implementations.

mod anthropic;
mod endpoint;
pub(crate) mod environment;
mod error;
mod frames;
mod litellm;
mod mistral;
pub(crate) mod model;
mod openai;
mod provider;
mod stream;
pub mod types;

pub use anthropic::Anthropic;
pub use error::{ProviderError, ProviderResult, RequestErrorKind};
pub use litellm::LiteLlm;
pub use mistral::Mistral;
pub use model::Model;
pub use openai::OpenAi;
pub use provider::{Provider, ProviderLike};
pub use types::{
    AsUserMessage, ContentBlock, Message, ModelRequest, ModelResponse, ReasoningEffort,
    ResponseStatus, StreamEvent, TokenUsage, ToolDeclineKind,
};

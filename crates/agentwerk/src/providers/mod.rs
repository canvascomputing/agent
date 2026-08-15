//! The `Provider` handle, the `ProviderLike` trait behind it, and the vendor-specific implementations that speak to Anthropic, OpenAI-compatible APIs, Mistral, and LiteLLM.
//!
//! The types a request and a response are made of (`Message`, `ContentBlock`,
//! `ModelRequest`, `ModelResponse`, `StreamEvent`) are reachable by name but
//! kept out of the index: they matter only when implementing [`ProviderLike`].

mod anthropic;
mod endpoint;
pub(crate) mod environment;
mod error;
mod framed_calls;
mod litellm;
mod mistral;
pub(crate) mod model;
mod openai;
mod patterns;
mod provider;
mod response;
mod stream;
pub mod types;

pub use anthropic::Anthropic;
pub use error::{ProviderError, ProviderResult, RequestErrorKind};
pub use litellm::LiteLlm;
pub use mistral::Mistral;
pub use model::Model;
pub use openai::OpenAi;
pub use provider::{
    ModelRequest, Provider, ProviderLike, ProviderToolDefinition, ReasoningEffort, ToolChoice,
};
pub use types::{
    AsUserMessage, ContentBlock, Message, ModelResponse, ResponseStatus, StreamEvent, TokenUsage,
    ToolDeclineKind,
};

//! End-to-end: a reasoning model streams its thinking back and the
//! provider assembles it into a `ContentBlock::Thinking` ahead of the
//! visible `Text`. Overrides the env model with `THINKING_MODEL` (a
//! reasoning-capable model, e.g. `claude-haiku-4-5`) and requests
//! `High` effort, so this exercises the reasoning-capture parse path
//! rather than the default chat model.

use std::sync::Arc;

use super::common;

use agentwerk::providers::{
    ContentBlock, Message, Model, ModelRequest, ReasoningEffort, StreamEvent,
};

#[tokio::test]
async fn test() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, default_model) = common::build_provider();
    let model = std::env::var("THINKING_MODEL")
        .map(Model::new)
        .unwrap_or(default_model);

    let request = ModelRequest {
        model: model.get_name().to_string(),
        system_prompt: String::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Is 91 prime? Think it through, then answer yes or no.".into(),
            }],
        }],
        tools: vec![],
        max_request_tokens: None,
        reasoning_effort: ReasoningEffort::High,
    };

    let sink: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(|_| {});
    let response = provider.respond(request, sink).await?;

    let thinking = response.content.iter().find_map(|b| match b {
        ContentBlock::Thinking { thinking, .. } => Some(thinking.clone()),
        _ => None,
    });
    let text = response.content.iter().find_map(|b| match b {
        ContentBlock::Text { text } => Some(text.clone()),
        _ => None,
    });

    eprintln!("blocks: {:?}", response.content);
    let thinking = thinking.expect("model should have streamed a Thinking block");
    assert!(
        !thinking.is_empty(),
        "Thinking block should carry reasoning"
    );
    // The Thinking block precedes the visible answer.
    assert!(matches!(
        response.content.first(),
        Some(ContentBlock::Thinking { .. })
    ));
    assert!(text.unwrap_or_default().to_lowercase().contains("no"));

    Ok(())
}

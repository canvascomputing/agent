//! Verifies a reasoning model streams a `ContentBlock::Thinking` before visible text. Set `THINKING_MODEL` to a reasoning-capable model; the test requests `High` effort.

use std::sync::Arc;

use super::common;

use agentwerk::providers::{
    ContentBlock, Message, Model, ModelRequest, ReasoningEffort, StreamEvent,
};

#[tokio::test]
async fn reasoning_effort_captures_thinking_before_visible_text(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
    assert!(matches!(
        response.content.first(),
        Some(ContentBlock::Thinking { .. })
    ));
    assert!(text.unwrap_or_default().to_lowercase().contains("no"));

    Ok(())
}

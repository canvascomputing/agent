//! End-to-end: a real LLM combines `GlobTool` and `ReadFileTool` to
//! explore a directory.

use super::common;

use agentwerk::event::EventName;
use agentwerk::tools::{GlobTool, ReadFileTool, TicketsTool};
use agentwerk::{Agent, Policy, TicketQueue};

#[tokio::test]
async fn test() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let tickets = TicketQueue::new();

    tickets.policy(Policy {
        max_turns: Some(10),
        ..Default::default()
    });
    let agent = Agent::new()
        .provider(provider)
        .model(&model)
        .role(
            "{context}\n\n\
             Explore the repository to answer the task. When you have an answer, \
             settle the ticket via `tickets` with `action: \"done\"` \
             and `result` set to your answer.",
        )
        .tool(ReadFileTool)
        .tool(GlobTool)
        .tool(TicketsTool)
        .build();
    tickets.agent(agent);
    tickets.ticket("Find all Rust source files and describe what this project does.");

    tickets.finish_all().await;
    common::print_result(&tickets);

    assert!(
        tickets
            .find_events(|e| e.kind.event_name() == EventName::ToolCallStarted)
            .len()
            >= 1
    );

    Ok(())
}

//! End-to-end: a real LLM combines `GlobTool` and `ReadFileTool` to
//! explore a directory.

use super::common;

use agentwerk::event::EventName;
use agentwerk::tools::{GlobTool, ReadFileTool, TicketsTool};
use agentwerk::{Agent, TicketQueue};

#[tokio::test]
async fn test() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let tickets = TicketQueue::new();

    tickets.max_turns(10);
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
    tickets.task("Find all Rust source files and describe what this project does.");

    tickets.finish_all().await;
    common::print_result(&tickets, tickets.stats());

    assert!(tickets.stats().event_count(EventName::ToolCallStarted) >= 1);

    Ok(())
}

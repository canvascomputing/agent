//! End-to-end: a real LLM combines `GlobTool` and `ReadFileTool` to
//! explore a directory.

use super::common;

use agentwerk::tools::{GlobTool, ReadFileTool, TaskTool};
use agentwerk::{Agent, Policy, Queue};

#[tokio::test]
async fn test() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let tasks = Queue::new();

    tasks.set_policy(Policy {
        max_turns: Some(10),
        ..Default::default()
    });
    let agent = Agent::new()
        .provider(provider)
        .model(&model)
        .role(
            "{context}\n\n\
             Explore the repository to answer the task. When you have an answer, \
             settle the task via `tasks` with `action: \"done\"` \
             and `result` set to your answer.",
        )
        .tool(ReadFileTool)
        .tool(GlobTool)
        .tool(TaskTool);
    tasks.add_agent(agent);
    tasks.add_task("Find all Rust source files and describe what this project does.");

    tasks.finish_all_tasks().await;
    common::print_result(&tasks);

    assert!(tasks.find_events("tool_call_started").len() >= 1);

    Ok(())
}

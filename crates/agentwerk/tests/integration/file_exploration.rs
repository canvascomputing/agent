//! Verifies a real LLM can explore a directory with `GlobTool` and `ReadFileTool`.

use super::common;

use agentwerk::tools::{GlobTool, ReadFileTool, TaskTool};
use agentwerk::{Agent, Policy, Werk};

#[tokio::test]
async fn file_tools_explore_the_repository() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let (provider, model) = common::build_provider();

    let werk = Werk::new();

    werk.set_policy(Policy {
        max_turns: Some(10),
        ..Default::default()
    });
    let agent = Agent::new()
        .provider(provider)
        .model(&model)
        .role(
            "{{ context }}\n\n\
             Explore the repository to answer the task. When you have an answer, \
             finish the task with your answer.",
        )
        .tool(ReadFileTool)
        .tool(GlobTool)
        .tool(TaskTool);
    werk.add_agent(agent);
    werk.add_task("Find all Rust source files and describe what this project does.");

    werk.finish().await;
    common::print_result(&werk);

    assert!(!werk
        .find_events("event.name = tool_call_started")
        .is_empty());

    Ok(())
}

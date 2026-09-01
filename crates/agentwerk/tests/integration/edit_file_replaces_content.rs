//! Verifies a real LLM can replace one substring with `EditFileTool` without changing the rest of the file.

use std::fs;

use super::common;

use agentwerk::tools::EditFileTool;
use agentwerk::{Agent, Policy, Werk};

const ORIGINAL: &str = "setting=old_value\nother=keep_me\n";

#[tokio::test]
async fn replaces_substring_in_place() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let dir = crate::test_util::TempDir::new()?;
    let root = dir.path();
    let path = root.join("config.txt");
    fs::write(&path, ORIGINAL)?;

    let werk = Werk::new();

    werk.set_policy(Policy {
        max_turns: Some(10),
        ..Default::default()
    });
    let agent = Agent::new()
        .provider(provider)
        .model(&model)
        .dir(root)
        .role(
            "{context}\n\n\
             Step 1: call `edit_file` to perform an exact substring \
             replacement in the existing file. Do not rewrite the whole file. \
             Step 2: immediately call `finish` to settle the \
             task. Do not write any prose: your only output must be tool \
             calls.",
        )
        .tool(EditFileTool);
    werk.add_agent(agent);
    werk.add_task(
        "In `config.txt`, change the substring `old_value` to `new_value`. \
         Leave the rest of the file untouched.",
    );

    werk.finish_all_tasks().await;
    common::print_result(&werk);

    assert!(
        !werk.find_events("tool_call_started").is_empty(),
        "agent must call at least one tool"
    );

    let content = fs::read_to_string(&path)?;
    assert!(
        content.contains("setting=new_value"),
        "expected `setting=new_value`; got:\n{content}"
    );
    assert!(
        content.contains("other=keep_me"),
        "untouched line `other=keep_me` was lost; got:\n{content}"
    );
    assert!(
        !content.contains("old_value"),
        "`old_value` should have been replaced; got:\n{content}"
    );

    Ok(())
}

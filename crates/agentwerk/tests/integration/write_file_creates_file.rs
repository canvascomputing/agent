//! Verifies a real LLM can create a file with `WriteFileTool`. The file contents provide the assertion without a result schema.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use super::common;

use agentwerk::tools::WriteFileTool;
use agentwerk::{Agent, Policy, Werk};

#[tokio::test]
async fn creates_file_with_token() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let token = ten_digit_token();
    let dir = crate::test_util::TempDir::new()?;
    let root = dir.path();

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
             Step 1: call `write_file` to create exactly the file the user \
             asks for, with exactly the content they specify (and nothing else). \
             Step 2: immediately call `finish` to settle the \
             task. Do not write any prose: your only output must be tool \
             calls.",
        )
        .tool(WriteFileTool);
    werk.add_agent(agent);
    werk.add_task(format!(
        "Create a file named `report.md` in the working directory containing \
         exactly the line `token={token}`."
    ));

    werk.finish().await;
    common::print_result(&werk);

    assert!(
        !werk
            .find_events("event.name = tool_call_started")
            .is_empty(),
        "agent must call at least one tool"
    );

    let path = root.join("report.md");
    assert!(
        path.exists(),
        "expected `report.md` to be created at {path:?}"
    );
    let content = fs::read_to_string(&path)?;
    assert!(
        content.contains(&format!("token={token}")),
        "report.md does not contain `token={token}`; got:\n{content}"
    );

    Ok(())
}

fn ten_digit_token() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u64;
    1_000_000_000 + (nanos.wrapping_mul(2_654_435_761) % 9_000_000_000)
}

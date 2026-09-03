//! Verifies a real LLM can use restricted `ls`, `cat`, and `wc` commands and return a schema-valid result.

use super::common;

use agentwerk::schemas::Schema;
use agentwerk::tools::CommandTool;
use agentwerk::{Agent, Policy, Task, Werk};

#[tokio::test]
async fn command_tools_produce_the_schema_bound_result(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let schema = Schema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "files": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Files in the working directory"
            },
            "line_count": {
                "type": "integer",
                "description": "Number of lines in Cargo.toml"
            }
        },
        "required": ["files", "line_count"]
    }))?;

    let ls = CommandTool::new("ls").allow("ls*").concurrent(true);
    let cat = CommandTool::new("cat").allow("cat *").concurrent(true);
    let wc = CommandTool::new("wc").allow("wc *").concurrent(true);

    let werk = Werk::new();

    werk.set_policy(Policy {
        max_turns: Some(10),
        ..Default::default()
    });
    let agent = Agent::new()
        .provider(provider)
        .model(&model)
        .role(
            "{context}\n\n\
             Step 1: call `ls`, `cat Cargo.toml`, and `wc -l Cargo.toml` to \
             gather the file list and Cargo.toml line count. \
             Step 2: immediately call `finish` with `result` \
             set to a JSON object in exactly this shape: \
             {\"files\": [\"<filename>\", ...], \"line_count\": <integer>}. \
             Pass the result as a JSON value, not a JSON-encoded string. \
             Never prose, never a sentence.",
        )
        .tool(ls)
        .tool(cat)
        .tool(wc);
    werk.add_agent(agent);
    werk.add_task(
        Task::new(
            "List the files in the current directory, read the Cargo.toml file, \
             and count its lines. Report the result.",
        )
        .schema(schema),
    );

    let json = werk
        .finish_task("ORDER BY task.created DESC")
        .await
        .unwrap_or_default();
    common::print_result(&werk);

    assert!(json["line_count"].as_u64().unwrap_or(0) > 1);
    assert!(json["files"].as_array().map_or(0, |a| a.len()) > 1);

    Ok(())
}

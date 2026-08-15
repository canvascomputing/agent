//! End-to-end: a real LLM drives three pattern-restricted `CommandTool`
//! commands (`ls`, `cat`, `wc`) and finishes its ticket with a
//! JSON result validated against the ticket schema.

use super::common;

use agentwerk::schemas::Schema;
use agentwerk::tools::CommandTool;
use agentwerk::{Agent, Ticket, TicketQueue};

#[tokio::test]
async fn test() -> std::result::Result<(), Box<dyn std::error::Error>> {
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

    let tickets = TicketQueue::new();

    tickets.max_turns(10);
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
        .tool(wc)
        .build();
    tickets.agent(agent);
    tickets.ticket(
        Ticket::new(
            "List the files in the current directory, read the Cargo.toml file, \
             and count its lines. Report the result.",
        )
        .schema(schema),
    );

    let json = tickets.finish_last().await.unwrap_or_default();
    common::print_result(&tickets);

    assert!(json["line_count"].as_u64().unwrap_or(0) > 1);
    assert!(json["files"].as_array().map_or(0, |a| a.len()) > 1);

    Ok(())
}

//! End-to-end: a tool is deferred, so the model sees only its name (empty
//! description, empty schema) in its definitions. A real LLM calls `find_tools`
//! to surface it, and we assert the tool's *output* carries the deferred tool's
//! full definition: name, the description (with its passphrase), and the
//! `passphrase` schema field that were all hidden until discovery. Tests that
//! `find_tools` renders a deferred definition the model could not otherwise see.

use std::sync::{Arc, Mutex};

use super::common;

use agentwerk::event::{Event, EventKind};
use agentwerk::tools::{FindToolsTool, Tool, ToolResult};
use agentwerk::{Agent, TicketQueue};
use serde_json::json;
use std::time::Duration;

#[derive(Clone)]
struct CapturedCall {
    name: String,
    output: Option<String>,
}

#[tokio::test]
async fn surfaces_a_deferred_tool_definition() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let (provider, model) = common::build_provider();

    // Deferred: the model sees only the name `vault_unlock_tool` until it runs
    // `find_tools`. The passphrase lives in the description and the `passphrase`
    // field lives in the schema: both hidden until surfaced.
    let vault = Tool::new(
        "vault_unlock_tool",
        "Unlock the vault. Required: pass `passphrase` set to exactly `open-sesame`.",
    )
    .schema(json!({
        "type": "object",
        "properties": {
            "passphrase": { "type": "string", "description": "Must be `open-sesame`." }
        },
        "required": ["passphrase"]
    }))
    .defer(true)
    .handler(|_input, _ctx| async { Ok(ToolResult::success("vault unlocked")) })
    .build();

    let calls: Arc<Mutex<Vec<CapturedCall>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);

    let tickets = TicketQueue::new();
    tickets.max_turns(6);
    tickets.max_time(Duration::from_secs(45));
    tickets.on_event(move |e: &Event| match &e.kind {
        EventKind::ToolCallStarted { tool_name, .. } => {
            sink.lock().unwrap().push(CapturedCall {
                name: tool_name.clone(),
                output: None,
            });
        }
        EventKind::ToolCallFinished {
            tool_name, output, ..
        } => {
            let mut g = sink.lock().unwrap();
            if let Some(slot) = g
                .iter_mut()
                .rev()
                .find(|c| &c.name == tool_name && c.output.is_none())
            {
                slot.output = Some(output.clone());
            }
        }
        _ => {}
    });
    tickets.agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .role(
                "{context}\n\n\
                 Do exactly what the user asks, one tool call per step, then call \
                 `finish`. Output tool calls only, never prose.",
            )
            .tool(FindToolsTool)
            .tool(vault)
            .build(),
    );
    tickets.task(
        "Step 1: call `find_tools` with `query` set to `vault` to reveal the \
         hidden vault tool. Step 2: call `finish`.",
    );

    let results = tickets.finish().await;
    common::print_result(results, tickets.stats());

    let recorded = calls.lock().unwrap().clone();
    let find = recorded
        .iter()
        .find(|c| c.name == "find_tools")
        .unwrap_or_else(|| {
            panic!(
                "model must call `find_tools` to reveal the deferred tool; calls: {:?}",
                recorded.iter().map(|c| &c.name).collect::<Vec<_>>()
            )
        });

    let output = find
        .output
        .as_deref()
        .expect("find_tools call should have produced output");
    // The surfaced definition must carry what the name-only stub omitted: the
    // deferred tool's name, its description text, and its schema field.
    for needle in ["vault_unlock_tool", "open-sesame", "passphrase"] {
        assert!(
            output.contains(needle),
            "find_tools output should surface `{needle}` from the deferred \
             definition; got: {output}"
        );
    }

    Ok(())
}

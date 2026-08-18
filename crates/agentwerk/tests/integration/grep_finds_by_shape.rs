//! End-to-end: a real LLM is given only `grep` and asked to list function
//! names it cannot know in advance. Proves `grep` answers a find-by-shape
//! question end-to-end: the model locates the definitions and reports the
//! names. (The code `syntax` is exercised directly in the unit tests; a
//! live model tends to reach for a regex `grep` here, and that is fine.)

use std::fs;
use std::sync::{Arc, Mutex};

use super::common;

use agentwerk::event::{default_logger, Event, EventKind};
use agentwerk::tools::GrepTool;
use agentwerk::{Agent, TicketQueue};

#[derive(Clone)]
struct CapturedCall {
    name: String,
    input: serde_json::Value,
}

#[tokio::test]
async fn grep_lists_unknown_function_names() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let (provider, model) = common::build_provider();

    let dir = crate::test_util::TempDir::new()?;
    let root = dir.path();

    // Three functions whose names the model is not told. A literal search
    // cannot list them; only a capturing metavariable surfaces each name.
    fs::write(
        root.join("geometry.rs"),
        "fn area(width: f64, height: f64) -> f64 { width * height }\n\
         fn perimeter(width: f64, height: f64) -> f64 { 2.0 * (width + height) }\n\
         fn clamp(value: f64) -> f64 { value.max(0.0) }\n",
    )?;

    let calls: Arc<Mutex<Vec<CapturedCall>>> = Arc::new(Mutex::new(Vec::new()));
    let collected = Arc::clone(&calls);
    let logger = default_logger();
    let event_handler = Arc::new(move |e: &Event| {
        if let EventKind::ToolCallStarted {
            tool_name, input, ..
        } = &e.kind
        {
            collected.lock().unwrap().push(CapturedCall {
                name: tool_name.clone(),
                input: input.clone(),
            });
        }
        logger(e);
    });

    let tickets = TicketQueue::new();

    tickets.max_turns(10);
    tickets.on_event(move |e| event_handler(e));
    tickets.agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .dir(root)
            .role(
                "{context}\n\n\
                 Investigate the working directory and answer the user's question. \
                 Use the available tools. When you have the answer, finish the ticket \
                 via `finish`.",
            )
            .tool(GrepTool)
            .build(),
    );
    tickets.ticket(
        "List the names of every function defined in `geometry.rs`. The names are \
         not known in advance. Answer with the names.",
    );

    tickets.finish_all().await;
    common::print_result(&tickets);

    let recorded = calls.lock().unwrap().clone();

    // The model must have used `grep` to locate the definitions.
    assert!(
        recorded.iter().any(|c| c.name == "grep"),
        "model should use `grep` to find the functions; instead called: {:?}",
        recorded
            .iter()
            .map(|c| (&c.name, &c.input))
            .collect::<Vec<_>>()
    );

    // The final answer should report every function name it discovered.
    let answer = common::last_result_text(&tickets);
    assert!(
        answer.contains("area") && answer.contains("perimeter") && answer.contains("clamp"),
        "model should report all three function names; got: {answer:?}"
    );

    Ok(())
}

//! End-to-end: a real LLM is asked to list the names of functions it
//! cannot know in advance, given ONLY `codegrep_tool`. The role does NOT
//! name the tool, describe its argument shape, or mention metavariables.
//! Proves the tool's *description* teaches the model to write a structural
//! query with a capturing metavariable (`fn $NAME(...)`) rather than a
//! literal substring, and to read the captured names out of the output.

use std::fs;
use std::sync::{Arc, Mutex};

use super::common;

use agentwerk::event::{default_logger, Event, EventKind};
use agentwerk::tools::CodegrepTool;
use agentwerk::{Agent, TicketSystem};

#[derive(Clone)]
struct CapturedCall {
    name: String,
    input: serde_json::Value,
    output: Option<String>,
}

#[tokio::test]
async fn writes_capturing_pattern_to_list_unknown_function_names(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
    let event_handler = Arc::new(move |e: Event| {
        match &e.kind {
            EventKind::ToolCallStarted {
                tool_name, input, ..
            } => {
                collected.lock().unwrap().push(CapturedCall {
                    name: tool_name.clone(),
                    input: input.clone(),
                    output: None,
                });
            }
            EventKind::ToolCallFinished {
                tool_name, output, ..
            } => {
                let mut g = collected.lock().unwrap();
                if let Some(slot) = g
                    .iter_mut()
                    .rev()
                    .find(|c| &c.name == tool_name && c.output.is_none())
                {
                    slot.output = Some(output.clone());
                }
            }
            _ => {}
        }
        logger(e);
    });

    let tickets = TicketSystem::new();

    tickets.max_turns(10);
    tickets.on_event(move |e| event_handler(e));
    tickets.agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .dir(root)
            .role(
                "Investigate the working directory and answer the user's question. \
                 Use the available tools. When you have the answer, finish the ticket \
                 via `finish_ticket`.",
            )
            .tool(CodegrepTool)
            .build(),
    );
    tickets.task(
        "List the names of every function defined in `geometry.rs`. The names are \
         not known in advance, so search by the shape of a function definition and \
         capture each name. Answer with the names.",
    );

    let results = tickets.finish().await;
    common::print_result(results, tickets.stats());

    let recorded = calls.lock().unwrap().clone();

    // The model must call codegrep_tool with a capturing metavariable, not a
    // literal substring: a `$` in the pattern is the structural-query signal.
    let codegrep_call = recorded
        .iter()
        .find(|c| {
            c.name == "codegrep_tool"
                && c.input
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .is_some_and(|p| p.contains('$'))
        })
        .unwrap_or_else(|| {
            panic!(
                "model should call `codegrep_tool` with a metavariable pattern \
                 (e.g. `fn $NAME(...)`); instead called: {:?}",
                recorded
                    .iter()
                    .map(|c| (&c.name, &c.input))
                    .collect::<Vec<_>>()
            )
        });

    let output = codegrep_call
        .output
        .as_deref()
        .expect("codegrep_tool call should have produced output");

    // The capturing pattern must surface the function names in the output.
    assert!(
        output.contains("area") && output.contains("perimeter") && output.contains("clamp"),
        "codegrep output should capture every function name; got: {output:?}"
    );

    // The model's final answer should report the names it discovered.
    let answer = common::last_result_text(results);
    assert!(
        answer.contains("area") && answer.contains("perimeter") && answer.contains("clamp"),
        "model should report all three function names; got: {answer:?}"
    );

    Ok(())
}

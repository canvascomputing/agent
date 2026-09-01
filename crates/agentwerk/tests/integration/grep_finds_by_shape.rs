//! Verifies a real LLM can use grep to discover and report function names it does not know in advance. Unit tests cover the code-specific syntax directly.

use std::fs;
use std::sync::{Arc, Mutex};

use super::common;

use agentwerk::event::{default_logger, Event};
use agentwerk::tools::GrepTool;
use agentwerk::{Agent, Policy, Werk};

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
        if e.get_name() == Event::TOOL_CALL_STARTED {
            let data = e.get_data();
            collected.lock().unwrap().push(CapturedCall {
                name: data["tool_name"].as_str().unwrap().to_string(),
                input: data["input"].clone(),
            });
        }
        logger(e);
    });

    let werk = Werk::new();

    werk.set_policy(Policy {
        max_turns: Some(10),
        ..Default::default()
    });
    werk.on_event(move |_, e| event_handler(e));
    werk.add_agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .dir(root)
            .role(
                "{context}\n\n\
                 Investigate the working directory and answer the user's question. \
                 Use the available tools. When you have the answer, finish the task \
                 via `finish`.",
            )
            .tool(GrepTool),
    );
    werk.add_task(
        "List the names of every function defined in `geometry.rs`. The names are \
         not known in advance. Answer with the names.",
    );

    werk.finish_all_tasks().await;
    common::print_result(&werk);

    let recorded = calls.lock().unwrap().clone();

    assert!(
        recorded.iter().any(|c| c.name == "grep"),
        "model should use `grep` to find the functions; instead called: {:?}",
        recorded
            .iter()
            .map(|c| (&c.name, &c.input))
            .collect::<Vec<_>>()
    );

    let answer = common::last_result_text(&werk);
    assert!(
        answer.contains("area") && answer.contains("perimeter") && answer.contains("clamp"),
        "model should report all three function names; got: {answer:?}"
    );

    Ok(())
}

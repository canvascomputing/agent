//! Shared setup for integration tests: provider construction from env
//! and a JSON result printer.

#![allow(dead_code)]

use agentwerk::event::Event;
use agentwerk::providers::{Model, Provider};
use agentwerk::Queue;

pub fn build_provider() -> (Provider, Model) {
    (
        Provider::from_env().expect("LLM provider required for integration tests"),
        Model::from_env().expect("model name required for integration tests"),
    )
}

/// The most recent result's text body, empty when absent or non-string.
pub fn last_result_text(tasks: &Queue) -> String {
    tasks
        .get_results()
        .pop()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub fn print_result(tasks: &Queue) {
    let recorded = tasks.find_events(|_: &Event| true);
    let count = |name: &str| {
        recorded
            .iter()
            .filter(|event| event.get_name() == name)
            .count()
    };
    let json = serde_json::json!({
        "response": tasks.get_results().pop().unwrap_or_default(),
        "turns": count(Event::TURN_STARTED),
        "tool_calls": count(Event::TOOL_CALL_STARTED),
        "tokens_in": tasks.get_input_tokens(),
        "tokens_out": tasks.get_output_tokens(),
    });
    eprintln!("{}", serde_json::to_string_pretty(&json).unwrap());
}

//! Provides live-provider setup and result printing for integration tests.

#![allow(dead_code)]

use agentwerk::event::Event;
use agentwerk::providers::{Model, Provider};
use agentwerk::Werk;

pub fn build_provider() -> (Provider, Model) {
    (
        Provider::from_env().expect("LLM provider required for integration tests"),
        Model::from_env().expect("model name required for integration tests"),
    )
}

/// The most recent result's text body, empty when absent or non-string.
pub fn last_result_text(werk: &Werk) -> String {
    werk.get_results()
        .pop()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub fn print_result(werk: &Werk) {
    let recorded = werk.find_events(|_: &Event| true);
    let count = |name: &str| {
        recorded
            .iter()
            .filter(|event| event.get_name() == name)
            .count()
    };
    let json = serde_json::json!({
        "response": werk.get_results().pop().unwrap_or_default(),
        "turns": count(Event::TURN_STARTED),
        "tool_calls": count(Event::TOOL_CALL_STARTED),
        "tokens_in": werk.get_input_tokens(),
        "tokens_out": werk.get_output_tokens(),
    });
    eprintln!("{}", serde_json::to_string_pretty(&json).unwrap());
}

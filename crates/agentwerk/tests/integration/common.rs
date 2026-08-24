//! Shared setup for integration tests: provider construction from env
//! and a JSON result printer.

#![allow(dead_code)]

use agentwerk::event::{Event, EventName};
use agentwerk::providers::{Model, Provider};
use agentwerk::TicketQueue;

pub fn build_provider() -> (Provider, Model) {
    (
        Provider::from_env().expect("LLM provider required for integration tests"),
        Model::from_env().expect("model name required for integration tests"),
    )
}

/// The most recent result's text body, empty when absent or non-string.
pub fn last_result_text(tickets: &TicketQueue) -> String {
    tickets
        .results()
        .pop()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub fn print_result(tickets: &TicketQueue) {
    let recorded = tickets.find_events(|_: &Event| true);
    let count = |kind: EventName| {
        recorded
            .iter()
            .filter(|e| e.kind.event_name() == kind)
            .count()
    };
    let json = serde_json::json!({
        "response": tickets.results().pop().unwrap_or_default(),
        "turns": count(EventName::TurnStarted),
        "tool_calls": count(EventName::ToolCallStarted),
        "tokens_in": tickets.input_tokens(),
        "tokens_out": tickets.output_tokens(),
    });
    eprintln!("{}", serde_json::to_string_pretty(&json).unwrap());
}

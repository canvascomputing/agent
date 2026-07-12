//! Shared setup for integration tests: provider construction from env
//! and a JSON result printer.

#![allow(dead_code)]

use std::sync::Arc;

use agentwerk::providers::{model_from_env, provider_from_env, Provider};
use agentwerk::{Stats, TicketSystem};

pub fn build_provider() -> (Arc<dyn Provider>, String) {
    let provider = provider_from_env().expect("LLM provider required for integration tests");
    let model = model_from_env().expect("model name required for integration tests");
    (provider, model)
}

/// The most recent result's text body, empty when absent or non-string.
pub fn last_result_text(tickets: &TicketSystem) -> String {
    tickets
        .last_result()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub fn print_result(tickets: &TicketSystem, stats: &Stats) {
    let json = serde_json::json!({
        "response": tickets.last_result().unwrap_or_default(),
        "turns": stats.turns(),
        "tool_calls": stats.tool_calls(),
        "tokens_in": stats.input_tokens(),
        "tokens_out": stats.output_tokens(),
    });
    eprintln!("{}", serde_json::to_string_pretty(&json).unwrap());
}

//! End-to-end: a real LLM uses `manage_tickets` with `action: "create"`
//! to add a new ticket to the queue. We verify a fresh ticket landed carrying
//! the requested body: the queue state is the assertion. The role does not
//! name the action shape; the tool's description must carry it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::common;

use agentwerk::tools::ManageTicketsTool;
use agentwerk::{Agent, TicketSystem};

#[tokio::test]
async fn creates_a_followup_ticket() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let token = ten_digit_token();
    let body = format!("batch-{token}");
    let instruction = format!(
        "Create one brand-new ticket in the queue: call the ticket tool with its \
         create action, setting the new ticket's task to exactly `{body}`. Do not \
         list or search. After the new ticket exists, call `finish`."
    );

    let tickets = TicketSystem::new();
    tickets.max_turns(10);
    tickets.max_time(Duration::from_secs(45));
    tickets.agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .role(
                "{context}\n\n\
                 Carry out the user's request using the ticket tools, then call \
                 `finish`. Create new tickets with the appropriate tool \
                 action; do not try to finish tickets other than your own.",
            )
            .tool(ManageTicketsTool)
            .build(),
    );
    tickets.task(instruction.clone());

    let results = tickets.finish().await;
    common::print_result(results, tickets.stats());

    assert!(
        tickets.stats().tool_calls() >= 1,
        "agent must call at least one tool"
    );

    // A new ticket must carry the token. The agent's own ticket holds the
    // instruction; the created one is any other ticket whose task, string or
    // structured JSON, mentions the token. The exact body shape is the
    // model's choice and not what this test pins down.
    let created = tickets.tickets().into_iter().find(|t| {
        t.task.as_str() != Some(instruction.as_str()) && t.task.to_string().contains(&body)
    });
    assert!(
        created.is_some(),
        "expected a new ticket whose body contains `{body}`; queue was: {:?}",
        tickets
            .tickets()
            .iter()
            .map(|t| t.task.clone())
            .collect::<Vec<_>>()
    );

    Ok(())
}

fn ten_digit_token() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u64;
    1_000_000_000 + (nanos.wrapping_mul(2_654_435_761) % 9_000_000_000)
}

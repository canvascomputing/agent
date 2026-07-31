//! End-to-end: a real LLM uses `read_tickets` to locate a ticket it does
//! not own and report a secret recorded only in that ticket's body. The secret
//! is absent from the agent's own task, so a correct answer proves the model
//! read the queue. The seeded ticket carries a label the agent does not handle,
//! so it stays `Todo` and unclaimed: readable but never executed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::common;

use agentwerk::tools::ReadTicketsTool;
use agentwerk::{Agent, Ticket, TicketQueue};

#[tokio::test]
async fn reports_a_secret_from_another_ticket(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let secret = ten_digit_token();

    let tickets = TicketQueue::new();
    tickets.max_turns(10);
    tickets.max_time(Duration::from_secs(45));
    // Seeded under a label the agent does not carry, so it is never claimed.
    tickets.ticket(
        Ticket::new(format!(
            "Maintenance note: the vault combination is {secret}. Do not share."
        ))
        .label("archive"),
    );
    tickets.agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .role(
                "{context}\n\n\
                 Answer the user by inspecting the ticket queue with the \
                 available ticket tool, then call `finish` with the \
                 answer. Do not guess: read the relevant ticket and quote it.",
            )
            .tool(ReadTicketsTool)
            .build(),
    );
    tickets.task(
        "Another ticket in the queue records a vault combination. Find that \
         ticket and report the exact combination number it contains.",
    );

    let results = tickets.finish().await;
    common::print_result(results, tickets.stats());

    assert!(
        tickets.stats().tool_calls() >= 1,
        "agent must call the ticket-reading tool at least once"
    );

    let answer = common::last_result_text(results);
    assert!(
        answer.contains(&secret.to_string()),
        "final result must quote the combination {secret} found only in the \
         seeded ticket; got: {answer}"
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

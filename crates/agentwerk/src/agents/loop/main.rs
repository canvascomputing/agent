//! Starts one tokio task per registered agent, decides when the run is over,
//! and waits for them on shutdown.

use crate::agents::tickets::{now_millis, TicketQueue};
use crate::event::{EventKind, FinishReason};

use super::agent::run_agent;
use super::POLL_INTERVAL;

/// Runs until nothing is left to work on, then names the ending exactly once.
/// Deciding here rather than in whichever caller happens to await means a
/// limit breached while the host is busy elsewhere still ends the run.
pub(in crate::agents) async fn run_main_loop(ticket_queue: &TicketQueue) {
    let mut running_agents: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut agents_already_started: usize = 0;

    while ticket_queue.run.is_working() {
        let registry = ticket_queue.clone_agents();
        for newly_registered_agent in registry.into_iter().skip(agents_already_started) {
            running_agents.push(tokio::spawn(run_agent(newly_registered_agent)));
            agents_already_started += 1;
        }
        if let Some(reason) = ticket_queue.ending_reason() {
            ticket_queue.run.set_draining(reason);
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for agent in running_agents {
        let _ = agent.await;
    }
    let reason = ticket_queue.run.reason().unwrap_or(FinishReason::Drained);
    ticket_queue.stats.record_execution_finished(now_millis());
    ticket_queue.emit("", "", EventKind::RunFinished { reason });
    // Last, so a caller that starts another run never overlaps this one.
    ticket_queue.run.set_finished();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agents::agent::Agent;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::{Status, Ticket, TicketQueue};
    use crate::event::EventKind;
    use crate::tools::TicketsTool;

    // Late-add agent tests

    #[tokio::test]
    async fn add_after_run_spawns_new_agent() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));

        let run_handle = tickets.start();

        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        tickets.agent(
            Agent::new()
                .label("late")
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(TicketsTool)
                .build(),
        );
        tickets.ticket(Ticket::new("hello").label("late"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let done = tickets
                .tickets()
                .iter()
                .any(|t| t.status == Status::Finished && t.task.as_str() == Some("hello"));
            if done {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                run_handle.finish_all().await;
                panic!("late-added agent did not finish ticket within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        run_handle.finish_all().await;

        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn host_finish_mid_run_walks_the_agent_off_and_still_drains() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));

        // The agent replies without calling its finish tool, so the ticket is
        // still in progress when the host resolves it out of band.
        let provider = MockProvider::with_results(vec![Ok(text_response("still working"))]);
        tickets.agent(
            Agent::new()
                .label("slow")
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .build(),
        );
        let key = tickets.ticket(Ticket::new("hello").label("slow"));

        // Resolved off the agent's own first turn rather than off a poll racing
        // it: polling made the host's win depend on scheduling, and losing it
        // left the agent's own outcome on the ticket instead.
        let host = Arc::clone(&tickets);
        let resolved = key.clone();
        tickets.on_event(move |event| {
            if matches!(event.kind, EventKind::RequestFinished { .. }) {
                let _ = host.set_finished(&resolved, "resolved by the host");
            }
        });

        tickets.finish_all().await;

        let ticket = tickets.get_ticket(&key).unwrap();
        assert_eq!(ticket.status, Status::Finished);
        assert_eq!(
            ticket.result,
            Some(serde_json::json!("resolved by the host"))
        );
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn late_added_agent_joined_on_shutdown() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));

        let run_handle = tickets.start();

        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        tickets.agent(
            Agent::new()
                .label("late")
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TicketsTool)
                .build(),
        );
        tickets.ticket(Ticket::new("x").label("late"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let done = tickets
                .tickets()
                .iter()
                .any(|t| t.status == Status::Finished && t.task.as_str() == Some("x"));
            if done {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                run_handle.finish_all().await;
                panic!("late-added agent did not finish ticket within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        tokio::time::timeout(Duration::from_secs(2), run_handle.finish_all())
            .await
            .expect("start() did not return within 2s of signal flip");
    }
}

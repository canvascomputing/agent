//! Main supervisor loop: spawns one tokio task per registered agent and joins them on shutdown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::agents::tickets::TicketSystem;

use super::agent::run_agent;
use super::POLL_INTERVAL;

pub(in crate::agents) async fn run_main_loop(ticket_system: &TicketSystem) {
    let shutdown_requested = Arc::clone(&ticket_system.stop_signal.lock().unwrap());
    let mut running_agents: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut agents_already_started: usize = 0;

    while !shutdown_requested.load(Ordering::Relaxed) {
        let registry = ticket_system.clone_agents();
        for newly_registered_agent in registry.into_iter().skip(agents_already_started) {
            running_agents.push(tokio::spawn(run_agent(newly_registered_agent)));
            agents_already_started += 1;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for agent in running_agents {
        let _ = agent.await;
    }
}

pub(super) async fn wait_for_signal(signal: &Arc<AtomicBool>) {
    loop {
        if signal.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agents::agent::Agent;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::{Status, Ticket, TicketSystem};
    use crate::providers::Provider;
    use crate::tools::ManageTicketsTool;

    // Late-add agent tests

    #[tokio::test]
    async fn add_after_run_spawns_new_agent() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));

        let run_handle = tickets.start();

        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        tickets.agent(
            Agent::new()
                .name("late")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(ManageTicketsTool)
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
                run_handle.finish().await;
                panic!("late-added agent did not finish ticket within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        run_handle.finish().await;

        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn late_added_agent_joined_on_shutdown() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));

        let run_handle = tickets.start();

        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        tickets.agent(
            Agent::new()
                .name("late")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(ManageTicketsTool)
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
                run_handle.finish().await;
                panic!("late-added agent did not finish ticket within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        tokio::time::timeout(Duration::from_secs(2), run_handle.finish())
            .await
            .expect("start() did not return within 2s of signal flip");
    }
}

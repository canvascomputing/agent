//! Starts one tokio task per registered agent, decides when the run is over,
//! and waits for them on shutdown.

use crate::agents::tasks::{FinishReason, Werk};
use crate::event::Event;

use super::POLL_INTERVAL;

/// Runs until nothing is left to work on, then names the ending exactly once.
/// Deciding here rather than in whichever caller happens to await means a
/// limit breached while the host is busy elsewhere still ends the run.
pub(in crate::agents) async fn run_main_loop(werk: &Werk) {
    let mut running_agents: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut agents_already_started: usize = 0;

    while werk.run.is_working() {
        let registry = werk.clone_agents();
        for newly_registered_agent in registry.into_iter().skip(agents_already_started) {
            running_agents.push(tokio::spawn(newly_registered_agent.run()));
            agents_already_started += 1;
        }
        if let Some(reason) = werk.ending_reason() {
            werk.run.set_draining(reason);
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for agent in running_agents {
        let _ = agent.await;
    }
    let reason = werk.run.reason().unwrap_or(FinishReason::Drained);
    werk.emit_event(Event::new(Event::RUN_FINISHED).data(serde_json::json!({ "outcome": reason })));
    // Last, so a caller that starts another run never overlaps this one.
    werk.run.set_finished();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::agents::agent::Agent;
    use crate::agents::policy::Policy;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::{Status, Task, Werk};
    use crate::event::Event;
    use crate::tools::TaskTool;

    #[tokio::test]
    async fn completion_waits_for_failure_handlers_and_wakes_when_they_finish() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf());
        let id = werk.add_task("go");
        let (entered, started) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        werk.on_failure(move |_, event, _| {
            if event.get_name() == Event::TASK_FAILED {
                entered.lock().unwrap().take().unwrap().send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }
        });
        let failing = werk.clone();
        let failure = std::thread::spawn(move || failing.set_task_failed(&id).unwrap());
        started.await.unwrap();
        let completion = werk.finish();
        tokio::pin!(completion);
        tokio::select! {
            biased;
            _ = &mut completion => panic!("completion returned before the failure hook"),
            _ = tokio::task::yield_now() => {},
        }
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), completion)
            .await
            .unwrap();
        failure.join().unwrap();
    }

    // Late-add agent tests

    #[tokio::test]
    async fn add_after_run_spawns_new_agent() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        let run_handle = werk.start();

        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        werk.add_agent(
            Agent::new()
                .label("late")
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(TaskTool),
        );
        werk.add_task(Task::new("hello").label("late"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let done = werk
                .get_tasks()
                .iter()
                .any(|t| t.status == Status::Finished && t.task.as_str() == Some("hello"));
            if done {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                run_handle.finish().await;
                panic!("late-added agent did not finish task within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        run_handle.finish().await;

        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn host_finish_mid_run_walks_the_agent_off_and_still_drains() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        // The agent replies without calling its finish tool, so the task is
        // still in progress when the host resolves it out of band.
        let provider = MockProvider::with_results(vec![Ok(text_response("still working"))]);
        werk.add_agent(
            Agent::new()
                .label("slow")
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );
        let id = werk.add_task(Task::new("hello").label("slow"));

        // Resolved off the agent's own first turn rather than off a poll racing
        // it: polling made the host's win depend on scheduling, and losing it
        // left the agent's own outcome on the task instead.
        let host = Arc::clone(&werk);
        let resolved = id.clone();
        werk.on_event(move |_, event| {
            if event.get_name() == Event::REQUEST_FINISHED {
                let _ = host.set_task_finished(
                    &resolved,
                    serde_json::json!({"answer": "resolved by the host"}),
                );
            }
        });

        werk.finish().await;

        let task = werk.get_task(&id).unwrap();
        assert_eq!(task.status, Status::Finished);
        assert_eq!(
            task.result,
            Some(serde_json::json!({"answer": "resolved by the host"}))
        );
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn late_added_agent_joined_on_shutdown() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        let run_handle = werk.start();

        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        werk.add_agent(
            Agent::new()
                .label("late")
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TaskTool),
        );
        werk.add_task(Task::new("x").label("late"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let done = werk
                .get_tasks()
                .iter()
                .any(|t| t.status == Status::Finished && t.task.as_str() == Some("x"));
            if done {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                run_handle.finish().await;
                panic!("late-added agent did not finish task within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        tokio::time::timeout(Duration::from_secs(2), run_handle.finish())
            .await
            .expect("start() did not return within 2s of signal flip");
    }
}

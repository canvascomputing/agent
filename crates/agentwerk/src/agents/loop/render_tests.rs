//! Task-level examples of when templates resolve and when text stays fixed.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::test_util::*;
use crate::agents::tasks::{Author, Reply};
use crate::providers::{
    Model, ModelRequest, ModelResponse, ProviderLike, ProviderResult, StreamEvent,
};
use crate::{Agent, Event, Policy, Task, Werk};

fn session() -> (Arc<Werk>, crate::test_util::TempDir) {
    let dir = crate::test_util::TempDir::new().unwrap();
    let werk = Werk::new();
    werk.set_dir(dir.path().to_path_buf()).on_event(|_, _| {});
    (werk, dir)
}

async fn finish(werk: &Werk) {
    tokio::time::timeout(Duration::from_secs(5), werk.finish())
        .await
        .unwrap();
}

fn research(werk: &Werk, result: &str) -> String {
    let id = werk.add_task(Task::labeled("research", "research"));
    werk.set_task_finished(&id, serde_json::json!(result))
        .unwrap();
    id
}

#[tokio::test]
async fn first_request_sees_values_and_results_supplied_after_the_task_is_added() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
    werk.add_agent(task_agent(&provider).role("{{ company }}: {{ result: research }}"));
    let id = werk.add_task("Write for {{ company }}: {{ result: research }}");
    assert_eq!(
        werk.get_task(&id).unwrap().get_task(),
        "Write for {{ company }}: {{ result: research }}"
    );
    werk.set_template("company", "Acme");
    research(&werk, "findings");
    finish(&werk).await;
    assert_eq!(provider.received_system_prompts(), ["Acme: findings"]);
    assert_eq!(
        user_text(&provider.received()[0]),
        "Write for Acme: findings\n"
    );
}

#[tokio::test]
async fn runtime_context_values_render_in_roles_and_string_tasks() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
    werk.add_agent(task_agent(&provider).role("Role {{ task_id }}"));
    let id = werk.add_task("Task {{ task_id }}");

    finish(&werk).await;

    assert_eq!(provider.received_system_prompts(), [format!("Role {id}")]);
    assert_eq!(user_text(&provider.received()[0]), format!("Task {id}\n"));
}

#[tokio::test]
async fn later_template_and_result_updates_do_not_change_the_task_prompts() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![
        Ok(text_response("continue")),
        Ok(write_result_response("done")),
    ]);
    let first = research(&werk, "old research");
    let first_path = werk.result_path(&first).canonicalize().unwrap();
    werk.set_template("company", "Old");
    let role = "{{ company }}: {{ result: research ORDER BY task.id DESC }} | {{ result_path: research ORDER BY task.id DESC }}";
    werk.add_agent(task_agent(&provider).role(role));
    let id = werk.add_task("{{ company }}: {{ result: research ORDER BY task.id DESC }}");
    werk.on_event(|werk, event| {
        if event.get_name() == Event::REQUEST_FINISHED && werk.find_results("research").len() == 1 {
            werk.set_template("company", "New");
            research(werk, "new research");
        }
    });
    finish(&werk).await;
    let frozen = format!("Old: old research | {}", first_path.display());
    assert_eq!(
        provider.received_system_prompts(),
        [frozen.clone(), frozen.clone()]
    );
    let requests = provider.received();
    assert_eq!(user_text(&requests[0]), "Old: old research\n");
    assert_eq!(user_text(&requests[0][..1]), user_text(&requests[1][..1]));
    let task = werk.get_task(&id).unwrap();
    let recorded: Vec<_> = task
        .get_replies()
        .iter()
        .filter(|reply| reply.author == Author::System)
        .flat_map(|reply| &reply.content)
        .filter_map(|content| content.get_text())
        .collect();
    assert_eq!(recorded, [frozen.as_str()]);
}

struct SuspendedProvider {
    inner: Arc<MockProvider>,
    first: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl ProviderLike for SuspendedProvider {
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        let response = self.inner.respond(request, on_event);
        Box::pin(async move {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            response.await
        })
    }
}

#[tokio::test]
async fn updates_during_an_in_flight_request_do_not_change_the_task_prompt() {
    let (werk, _dir) = session();
    let recorded = MockProvider::with_results(vec![
        Ok(text_response("continue")),
        Ok(write_result_response("done")),
    ]);
    let provider = Arc::new(SuspendedProvider {
        inner: recorded.clone(),
        first: AtomicBool::new(true),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    werk.set_template("company", "Old");
    werk.add_agent(
        Agent::new()
            .provider(provider.clone())
            .model("mock")
            .role("{{ company }}"),
    );
    werk.add_task("go");
    let update = async {
        provider.entered.notified().await;
        assert_eq!(recorded.received_system_prompts(), ["Old"]);
        werk.set_template("company", "New");
        provider.release.notify_one();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(werk.finish(), update);
    })
    .await
    .unwrap();
    assert_eq!(recorded.received_system_prompts(), ["Old", "Old"]);
}

#[tokio::test]
async fn retry_and_following_requests_reuse_the_task_prompt() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![
        Err(rate_limit()),
        Ok(text_response("continue")),
        Ok(write_result_response("done")),
    ]);
    werk.set_policy(Policy {
        request_retry_delay: Duration::ZERO,
        max_request_retries: 1,
        ..Default::default()
    });
    werk.set_template("company", "Old");
    werk.add_agent(task_agent(&provider).role("{{ company }}"));
    werk.add_task("{{ company }}");
    werk.on_event(|werk, event| {
        if event.get_name() == Event::REQUEST_RETRIED {
            werk.set_template("company", "New");
        }
    });
    finish(&werk).await;
    assert_eq!(provider.received_system_prompts(), ["Old", "Old", "Old"]);
    assert_eq!(
        serde_json::to_value(&provider.received()[0]).unwrap(),
        serde_json::to_value(&provider.received()[1]).unwrap()
    );
}

#[tokio::test]
async fn template_updates_after_the_first_request_cannot_change_or_fail_the_task() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![
        Ok(text_response("continue")),
        Ok(write_result_response("done")),
    ]);
    werk.set_template("company", "Old");
    werk.add_agent(task_agent(&provider).role("{{ company }}"));
    let id = werk.add_task("go");
    werk.on_event(|werk, event| {
        if event.get_name() == Event::REQUEST_FINISHED {
            werk.set_template("company", "{{ result: absent }}");
        }
    });

    finish(&werk).await;

    assert_eq!(provider.received_system_prompts(), ["Old", "Old"]);
    let task = werk.get_task(&id).unwrap();
    assert!(task.is_finished());
    assert!(task
        .get_errors()
        .iter()
        .all(|error| error.get_name() != Event::PROMPT_RENDER_FAILED));
}

#[tokio::test]
async fn shared_values_with_expressions_remain_literal_across_requests() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![
        Ok(text_response("continue")),
        Ok(write_result_response("done")),
    ]);
    werk.set_template("data", "{{ company }} {{ result: absent }}");
    werk.add_agent(task_agent(&provider).role("Data: {{ data }}"));
    werk.add_task("Data: {{ data }}");
    finish(&werk).await;
    assert_eq!(
        provider.received_system_prompts(),
        [
            "Data: {{ company }} {{ result: absent }}",
            "Data: {{ company }} {{ result: absent }}"
        ]
    );
    assert_eq!(
        user_text(&provider.received()[0]),
        "Data: {{ company }} {{ result: absent }}\n"
    );
}

#[tokio::test]
async fn agent_setters_update_shared_values_for_roles_and_previously_added_tasks() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
    werk.set_template("company", "Werk");
    let creator = task_agent(&provider)
        .label("creator")
        .template("company", "old")
        .templates([("company", "Creator")]);
    werk.add_agent(creator.clone());
    werk.add_agent(
        task_agent(&provider)
            .label("worker")
            .role("{{ company }}")
            .template("company", "old")
            .template("company", "Worker"),
    );
    creator.add_task(Task::labeled("worker", "{{ company }}"));
    creator.clone().template("company", "Updated");
    finish(&werk).await;
    assert_eq!(provider.received_system_prompts(), ["Updated"]);
    assert_eq!(user_text(&provider.received()[0]), "Updated\n");
}

#[tokio::test]
async fn interactive_continuation_reuses_the_prompt_without_rendering_caller_replies() {
    let (werk, _dir) = session();
    let provider =
        MockProvider::with_results(vec![Ok(text_response("hello")), Ok(text_response("again"))]);
    werk.set_template("company", "Old");
    werk.add_agent(interactive_chatbot(&provider).role("{{ company }}"));
    let id = werk.add_task("{{ company }}");
    finish(&werk).await;
    werk.set_template("company", "New");
    werk.add_reply(&id, "{{ company }} {{ result: absent }}");
    finish(&werk).await;
    assert_eq!(provider.received_system_prompts(), ["Old", "Old"]);
    assert!(user_text(&provider.received()[1]).contains("{{ company }} {{ result: absent }}"));
    assert_eq!(
        user_text(&provider.received()[0][..1]),
        user_text(&provider.received()[1][..1])
    );
}

#[tokio::test]
async fn reload_uses_only_shared_templates_restored_by_the_caller() {
    let (werk, dir) = session();
    let provider = MockProvider::with_results(vec![
        Ok(write_result_response("one")),
        Ok(write_result_response("two")),
    ]);
    let agent = task_agent(&provider).template("company", "Captured");
    werk.add_agent(agent.clone());
    agent.add_task("{{ company }}");
    werk.set_template("data", "{{ company }} {{ result: absent }}");
    agent.add_task("{{ data }}");
    let loaded = Werk::load(dir.path()).unwrap();
    loaded
        .set_templates([
            ("company", "New"),
            ("data", "{{ company }} {{ result: absent }}"),
        ])
        .add_agent(task_agent(&provider));
    finish(&loaded).await;
    assert_eq!(user_text(&provider.received()[0]), "New\n");
    assert_eq!(
        user_text(&provider.received()[1]),
        "{{ company }} {{ result: absent }}\n"
    );
}

async fn fail_to_render(
    in_role: bool,
    prompt: &str,
) -> (
    Arc<Werk>,
    crate::test_util::TempDir,
    Arc<MockProvider>,
    String,
    Arc<Mutex<Vec<String>>>,
) {
    let (werk, dir) = session();
    let provider = MockProvider::with_results(vec![]);
    let role = if in_role { prompt } else { "role" };
    werk.add_agent(task_agent(&provider).role(role));
    let id = werk.add_task(if in_role { "go" } else { prompt });
    let failures = Arc::new(Mutex::new(Vec::new()));
    let observed = failures.clone();
    werk.on_failure(move |_, event, _| observed.lock().unwrap().push(event.get_name().to_string()));
    finish(&werk).await;
    (werk, dir, provider, id, failures)
}

#[tokio::test]
async fn role_and_task_render_failures_stop_before_the_provider_request() {
    for in_role in [true, false] {
        let (werk, _dir, provider, id, _failures) =
            fail_to_render(in_role, "{{ result: absent }}").await;

        assert_eq!(provider.requests(), 0);
        assert!(werk.get_task(&id).unwrap().is_failed());
    }
}

#[tokio::test]
async fn prompt_render_failures_reach_hooks_and_survive_reload() {
    let (_werk, dir, _provider, id, failures) = fail_to_render(true, "{{ result: absent }}").await;

    assert_eq!(
        *failures.lock().unwrap(),
        [Event::PROMPT_RENDER_FAILED, Event::TASK_FAILED]
    );
    let loaded = Werk::load(dir.path()).unwrap();
    let task = loaded.get_task(&id).unwrap();
    let error = &task.get_errors()[0];
    assert_eq!(error.get_name(), Event::PROMPT_RENDER_FAILED);
    assert_eq!(error.get_data()["expression"], "result: absent");
}

#[tokio::test]
async fn nested_render_failures_report_the_outer_expression() {
    let (werk, _dir, _provider, id, _failures) =
        fail_to_render(true, "{{ result: {{ missing_selection }} }}").await;

    let task = werk.get_task(&id).unwrap();
    let error = &task.get_errors()[0];
    assert_eq!(error.get_name(), Event::PROMPT_RENDER_FAILED);
    assert_eq!(
        error.get_data()["expression"],
        "result: {{ missing_selection }}"
    );
}

#[tokio::test]
async fn concurrent_tasks_receive_their_own_runtime_prompt_values() {
    let (werk, _dir) = session();
    let alpha = MockProvider::with_results(vec![Ok(write_result_response("alpha"))]);
    let beta = MockProvider::with_results(vec![Ok(write_result_response("beta"))]);
    werk.add_agent(task_agent(&alpha).label("alpha").role("{{ task_id }}"));
    werk.add_agent(task_agent(&beta).label("beta").role("{{ task_id }}"));
    let alpha_id = werk.add_task(Task::labeled("alpha", "go"));
    let beta_id = werk.add_task(Task::labeled("beta", "go"));
    finish(&werk).await;
    assert_eq!(alpha.received_system_prompts(), [alpha_id]);
    assert_eq!(beta.received_system_prompts(), [beta_id]);
}

#[tokio::test]
async fn structured_task_input_is_never_interpreted_as_a_template() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
    werk.set_template("company", "New")
        .add_agent(task_agent(&provider));
    werk.add_task(Task::new(
        serde_json::json!({"company": "{{ company }}", "query": "{{ result: absent }}"}),
    ));
    finish(&werk).await;
    let messages = provider.received();
    let input: serde_json::Value = serde_json::from_str(user_text(&messages[0]).trim()).unwrap();
    assert_eq!(input["company"], "{{ company }}");
    assert_eq!(input["query"], "{{ result: absent }}");
}

#[tokio::test]
async fn resumed_session_reuses_the_prompt_and_preserves_existing_messages() {
    let (werk, dir) = session();
    let provider =
        MockProvider::with_results(vec![Ok(text_response("hello")), Ok(text_response("again"))]);
    let agent = interactive_chatbot(&provider).role("{{ company }}");
    werk.set_template("company", "Old").add_agent(agent.clone());
    let id = werk.add_task("{{ company }}");
    finish(&werk).await;
    werk.cancel_all_tasks();
    finish(&werk).await;
    drop(werk);
    let loaded = Werk::load(dir.path()).unwrap();
    loaded.set_template("company", "New").add_agent(agent);
    loaded.add_reply(&id, "continue");
    finish(&loaded).await;
    assert_eq!(provider.received_system_prompts(), ["Old", "Old"]);
    assert_eq!(
        user_text(&provider.received()[0][..1]),
        user_text(&provider.received()[1][..1])
    );
}

#[tokio::test]
async fn resumed_session_does_not_record_an_unchanged_system_prompt_twice() {
    let (werk, dir) = session();
    let provider =
        MockProvider::with_results(vec![Ok(text_response("hello")), Ok(text_response("again"))]);
    let agent = interactive_chatbot(&provider).role("reviewer");
    werk.add_agent(agent.clone());
    let id = werk.add_task("review");
    finish(&werk).await;
    werk.cancel_all_tasks();
    finish(&werk).await;
    drop(werk);

    let loaded = Werk::load(dir.path()).unwrap();
    loaded.add_agent(agent).add_reply(&id, "continue");
    finish(&loaded).await;

    let system_replies = loaded
        .get_task(&id)
        .unwrap()
        .get_replies()
        .iter()
        .filter(|reply| reply.get_author() == Author::System)
        .count();
    assert_eq!(system_replies, 1);
}

#[tokio::test]
async fn legacy_histories_reuse_the_earliest_system_prompt_without_rewriting_replies() {
    let (werk, dir) = session();
    let provider =
        MockProvider::with_results(vec![Ok(text_response("hello")), Ok(text_response("again"))]);
    let agent = interactive_chatbot(&provider).role("{{ company }}");
    werk.set_template("company", "Old");
    werk.add_agent(agent.clone());
    let id = werk.add_task("review");
    finish(&werk).await;

    werk.cancel_all_tasks();
    finish(&werk).await;
    werk.append_reply(&id, Reply::system_text("Legacy refresh"));
    drop(werk);

    let loaded = Werk::load(dir.path()).unwrap();
    loaded.set_template("company", "New").add_agent(agent);
    loaded.add_reply(&id, "continue");
    finish(&loaded).await;

    assert_eq!(provider.received_system_prompts(), ["Old", "Old"]);
    let task = loaded.get_task(&id).unwrap();
    let system_replies: Vec<_> = task
        .get_replies()
        .iter()
        .filter(|reply| reply.get_author() == Author::System)
        .flat_map(|reply| reply.get_content())
        .filter_map(|content| content.get_text())
        .collect();
    assert_eq!(system_replies, ["Old", "Legacy refresh"]);
}

#[tokio::test]
async fn compaction_reuses_the_frozen_system_prompt() {
    let (werk, _dir) = session();
    let provider = MockProvider::with_results(vec![
        Ok(tool_call_response_with_usage(
            "primer",
            crate::providers::types::TokenUsage {
                input_tokens: 180_000,
                output_tokens: 0,
            },
        )),
        Ok(text_response_with_usage(
            "SUMMARY",
            crate::providers::types::TokenUsage::default(),
        )),
        Ok(write_result_response("done")),
    ]);
    werk.set_policy(Policy {
        max_schema_retries: Some(10),
        max_time: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    werk.set_template("company", "Old");
    werk.add_agent(
        Agent::new()
            .provider(provider.clone())
            .model(Model::new("mock").context_window(200_000))
            .role("{{ company }}"),
    );
    let id = werk.add_task(Task::new("go").schema(string_schema()));
    werk.on_event(|werk, event| {
        if event.get_name() == Event::REQUEST_FINISHED {
            werk.set_template("company", "New");
        }
    });

    finish(&werk).await;

    let prompts = provider.received_system_prompts();
    assert_eq!(prompts.len(), 3);
    assert_eq!(prompts.first().map(String::as_str), Some("Old"));
    assert_eq!(prompts.last().map(String::as_str), Some("Old"));
    let system_replies = werk
        .get_task(&id)
        .unwrap()
        .get_replies()
        .iter()
        .filter(|reply| reply.get_author() == Author::System)
        .count();
    assert_eq!(system_replies, 1);
}

#[tokio::test]
async fn standalone_and_shared_agents_use_the_same_templates_and_transfer_queued_sources() {
    let provider = MockProvider::with_results(vec![
        Ok(write_result_response("private")),
        Ok(write_result_response("shared")),
    ]);
    let (shared, _dir) = session();
    shared.set_template("brief", "Destination");
    let make_agent = || {
        task_agent(&provider)
            .template("brief", "Private")
            .role("{{ brief }}")
    };
    let agent = make_agent();
    let private = agent.werk.upgrade().unwrap();
    let private_dir = crate::test_util::TempDir::new().unwrap();
    private
        .set_dir(private_dir.path().to_path_buf())
        .on_event(|_, _| {});
    agent.add_task("{{ brief }}");
    agent.finish().await;
    private.add_agent(agent.clone());
    assert_eq!(private.get_tasks().len(), 1);
    assert!(private.get_tasks()[0].is_finished());
    assert_eq!(provider.received_system_prompts(), ["Private"]);
    let agent = make_agent();
    agent.add_task("{{ brief }}");
    shared.add_agent(agent.clone());
    assert!(!serde_json::to_value(shared.get_tasks().last().unwrap())
        .unwrap()
        .as_object()
        .unwrap()
        .contains_key("templates"));
    agent.clone().template("brief", "Updated");
    finish(&shared).await;
    assert_eq!(provider.received_system_prompts(), ["Private", "Updated"]);
    assert_eq!(user_text(&provider.received()[1]), "Updated\n");
}

#[test]
fn legacy_prompt_fields_are_ignored() {
    let (werk, _dir) = session();
    werk.set_template("company", "Shared");
    let mut data = serde_json::to_value(Task::from("{{ company }}")).unwrap();
    data["templates"] = serde_json::json!({"company": "Captured"});
    data["prompt_templates"] = serde_json::json!({"company": "Local"});
    data["rendered"] = true.into();
    let task: Task = serde_json::from_value(data).unwrap();
    assert_eq!(
        task.initial_reply(&werk, &[]).unwrap().content[0].get_text(),
        Some("Shared")
    );
}

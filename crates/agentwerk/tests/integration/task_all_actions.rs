//! Verifies a real LLM can read, list, query, create, and edit tasks from intent-only instructions. A real handover provides the parent result, and the role does not name actions or query syntax.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::common;

use agentwerk::tools::TaskTool;
use agentwerk::{Agent, Event, Policy, Query, Task, Werk};

const ACTIONS: [&str; 5] = ["task", "result", "list", "create", "edit"];

/// Sits unlabeled and unclaimed: both agents carry a label, so neither handles
/// it. It exists to give `list` something to find.
const DORMANT_NOTE: &str = "Quarterly inventory of the sealed archive room.";

#[tokio::test]
async fn walks_every_task_action() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let secret = ten_digit_token();

    let werk = Werk::new();
    werk.set_policy(Policy {
        max_turns: Some(20),
        max_time: Some(Duration::from_secs(120)),
        ..Default::default()
    });

    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let written = Arc::new(Mutex::new(Vec::new()));
    let actions = Arc::clone(&seen);
    let queries = Arc::clone(&written);
    werk.on_event(move |_, e| {
        if e.get_name() == Event::TOOL_CALL_STARTED {
            let data = e.get_data();
            if data["tool_name"] == "task" {
                let input = &data["input"];
                if let Some(action) = input["action"].as_str() {
                    actions.lock().unwrap().insert(action.to_string());
                }
                if let Some(aql) = input["aql"].as_str() {
                    queries.lock().unwrap().push(aql.to_string());
                }
            }
        }
    });

    werk.add_agent(
        Agent::new()
            .provider(provider.clone())
            .model(&model)
            .label("archive")
            .role(
                "{context}\n\n\
                 Finish your task in a single call: pass the combination from \
                 your task as your result, hand the work over to `auditor`, and \
                 give the new task the task `Audit the archived record.`",
            ),
    );
    werk.add_agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .label("auditor")
            .role(
                "{context}\n\n\
                 Work your task with the task tool, one call at a time, in \
                 this order, then call `finish`:\n\
                 1. Read your own task and note the parent it names.\n\
                 2. Read what that parent produced: it holds a vault combination.\n\
                 3. Find out how many tasks the Werk holds right now.\n\
                 4. Without reading the whole Werk again, ask it for the one \
                 task whose body carries the phrase `sealed archive room`.\n\
                 5. File a new task recording the combination, in the \
                 `records` scope so nobody picks it up.\n\
                 6. Correct the wording of that new task.\n\
                 Finish by quoting the exact combination.",
            )
            .tool(TaskTool),
    );

    werk.add_task(Task::new(DORMANT_NOTE));
    werk.add_task(
        Task::new(format!(
            "The vault combination is {secret}. Archive it and pass the audit on."
        ))
        .label("archive"),
    );

    // Not `finish_all_tasks`: the decoy and the record the agent files are labelled
    // for nobody, so they stay `todo` and a wait on the whole Werk only ever
    // ends at the time cap.
    werk.finish_tasks("task.label IN (archive, auditor)").await;
    common::print_result(&werk);

    let used = seen.lock().unwrap().clone();
    let missing: Vec<&str> = ACTIONS
        .iter()
        .copied()
        .filter(|a| !used.contains(*a))
        .collect();
    assert!(
        missing.is_empty(),
        "no intent steered the model to {missing:?}; it reached for {used:?}"
    );

    // A rejected query is answered and retried, so one that compiles is what
    // the intent has to reach, not every attempt on the way there.
    let written = written.lock().unwrap().clone();
    assert!(
        written.iter().any(|q| Query::new(q).is_ok()),
        "the narrowing intent reached no query that compiles; it wrote {written:?}"
    );

    let audit = werk
        .find_task("task.label = auditor AND task.status = finished")
        .expect("the auditor must finish the task handed to it");
    let answer = audit.get_result().cloned().unwrap_or_default().to_string();
    assert!(
        answer.contains(&secret.to_string()),
        "the answer must quote the combination {secret}, which only the parent's \
         result carries; got: {answer}"
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

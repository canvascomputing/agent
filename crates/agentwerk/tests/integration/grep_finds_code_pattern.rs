//! End-to-end: a real LLM is asked to find the file that contains a
//! specific code pattern with regex-special characters (`<`, `>`, `(`,
//! `)`, `,`, `:`). The role does NOT name `grep`, does NOT describe
//! its argument shape, and does NOT mention regex semantics. `grep` treats
//! `pattern` as a regular expression, so the `(` and `)` in the signature
//! must be escaped to match literally; left raw they act as groups and the
//! search zeroes out. Proves the tool's *description* is good enough for a
//! model to pick content search and escape the metacharacters correctly.

use std::fs;
use std::sync::{Arc, Mutex};

use super::common;

use agentwerk::event::{default_logger, Event, EventKind};
use agentwerk::tools::{GlobTool, GrepTool, ListDirectoryTool, ReadFileTool};
use agentwerk::{Agent, Policy, Queue};

/// The exact substring the model must locate. Contains regex metachars
/// (`(`, `)`) that the model must escape to match literally; left raw they
/// act as groups and the search finds nothing.
const TARGET_SIGNATURE: &str = "fn calculate(items: Vec<(String, i32)>)";

#[derive(Clone)]
struct CapturedCall {
    name: String,
    input: serde_json::Value,
    output: Option<String>,
}

#[tokio::test]
async fn finds_code_pattern_with_special_chars(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let dir = crate::test_util::TempDir::new()?;
    let root = dir.path();
    fs::create_dir_all(root.join("src"))?;

    // Three look-alike signatures plus the target. Only one file contains
    // the exact substring `fn calculate(items: Vec<(String, i32)>)`.
    fs::write(
        root.join("src/render.rs"),
        "pub fn render(node: &Node<'a>) -> Result<String, Error> { todo!() }\n",
    )?;
    fs::write(
        root.join("src/merge.rs"),
        "pub fn merge(items: Vec<(String, String)>) -> Vec<String> { todo!() }\n",
    )?;
    fs::write(
        root.join("src/process.rs"),
        "pub fn process(items: Vec<String>) -> Result<i32, Error> { todo!() }\n",
    )?;
    fs::write(
        root.join("src/calc.rs"),
        format!("pub {TARGET_SIGNATURE} -> Result<i32, Error> {{ Ok(0) }}\n"),
    )?;
    fs::write(
        root.join("README.md"),
        "# project\n\nSome notes about Vec and tuples.\n",
    )?;

    let calls: Arc<Mutex<Vec<CapturedCall>>> = Arc::new(Mutex::new(Vec::new()));
    let collected = Arc::clone(&calls);
    let logger = default_logger();
    let event_handler = Arc::new(move |e: &Event| {
        match &e.kind {
            EventKind::ToolCallStarted {
                tool_name, input, ..
            } => {
                collected.lock().unwrap().push(CapturedCall {
                    name: tool_name.clone(),
                    input: input.clone(),
                    output: None,
                });
            }
            EventKind::ToolCallFinished {
                tool_name, output, ..
            } => {
                let mut g = collected.lock().unwrap();
                if let Some(slot) = g
                    .iter_mut()
                    .rev()
                    .find(|c| &c.name == tool_name && c.output.is_none())
                {
                    slot.output = Some(output.clone());
                }
            }
            _ => {}
        }
        logger(e);
    });

    let tasks = Queue::new();

    tasks.set_policy(Policy {
        max_turns: Some(10),
        ..Default::default()
    });
    tasks.on_event(move |_, e| event_handler(e));
    tasks.add_agent(
        Agent::new()
            .provider(provider)
            .model(&model)
            .dir(root)
            .role(
                "{context}\n\n\
                 Investigate the working directory and answer the user's question. \
                 Use the available tools: pick whichever one fits the question. \
                 When you have the answer, settle the task via \
                 `finish`.",
            )
            .tool(GrepTool)
            .tool(GlobTool)
            .tool(ListDirectoryTool)
            .tool(ReadFileTool),
    );
    tasks.add_task(format!(
        "Which source file in this project contains the exact code \
         `{TARGET_SIGNATURE}`? Answer with the file's path."
    ));

    tasks.finish_all_tasks().await;
    common::print_result(&tasks);

    let recorded = calls.lock().unwrap().clone();

    // The model must have located the signature with `grep`: a call whose output
    // names calc.rs. Under a regex `pattern`, that hit is only reachable if the
    // model escaped the `(` and `)`: left raw they are groups and match nothing.
    let grep_hit = recorded
        .iter()
        .find(|c| {
            c.name == "grep"
                && c.output
                    .as_deref()
                    .is_some_and(|out| out.contains("calc.rs"))
        })
        .unwrap_or_else(|| {
            panic!(
                "model should locate the signature with `grep` (its output naming \
                 calc.rs); instead called: {:?}",
                recorded
                    .iter()
                    .map(|c| (&c.name, &c.input))
                    .collect::<Vec<_>>()
            )
        });

    // The signature is unique to calc.rs, so a correct search excludes the
    // look-alikes: a raw-paren regex would instead match none or the wrong tuple.
    let output = grep_hit.output.as_deref().unwrap_or("");
    assert!(
        !output.contains("merge.rs")
            && !output.contains("process.rs")
            && !output.contains("render.rs"),
        "grep output should NOT match the look-alike signatures; got: {output:?}"
    );

    // The agent's final answer should name calc.rs.
    let answer = common::last_result_text(&tasks);
    assert!(
        answer.contains("calc.rs"),
        "agent should report calc.rs; got: {answer:?}"
    );

    Ok(())
}

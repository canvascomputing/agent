//! Research one question through planning, parallel evidence gathering, and writing.
//!
//! Usage: deep-research <QUESTION>

mod web_search;

use std::sync::Arc;

use agentwerk::providers::{Model, Provider};
use agentwerk::schemas::Schema;
use agentwerk::tools::FetchTool;
use agentwerk::{Agent, Event, Task, Werk};
use serde_json::{json, Value};

use web_search::{brave_key_from_env, brave_search_tool};

const PLANNER_ROLE: &str = include_str!("prompts/planner.role.md");
const RESEARCHER_ROLE: &str = include_str!("prompts/researcher.role.md");
const WRITER_ROLE: &str = include_str!("prompts/writer.role.md");

const PLANNING: &str = "planning";
const RESEARCH: &str = "research";
const REPORT: &str = "report";
const ANGLES: usize = 3;
const RESEARCHERS: usize = 2;

#[tokio::main]
async fn main() {
    let question = question_from_args();
    let brave_key = brave_key_from_env().unwrap_or_else(|message| exit(&message));
    let provider = Provider::from_env().unwrap_or_else(|error| exit(&error.to_string()));
    let model = Model::from_env().unwrap_or_else(|error| exit(&error.to_string()));

    let werk = Werk::new();
    werk.set_dir(
        std::env::temp_dir().join(format!("agentwerk-deep-research-{}", std::process::id())),
    );
    werk.on_event(|_, event| log_progress(event));
    cancel_on_ctrl_c(Arc::clone(&werk));

    werk.add_agent(
        Agent::new()
            .provider(provider.clone())
            .model(model.clone())
            .label(PLANNING)
            .role(PLANNER_ROLE),
    );
    for _ in 0..RESEARCHERS {
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model(model.clone())
                .label(RESEARCH)
                .role(RESEARCHER_ROLE)
                .tool(brave_search_tool(brave_key.clone()))
                .tool(FetchTool::new()),
        );
    }
    werk.add_agent(
        Agent::new()
            .provider(provider)
            .model(model)
            .label(REPORT)
            .role(WRITER_ROLE),
    );

    eprintln!("Planning three research angles…");
    werk.add_task(
        Task::new(json!({ "question": question }))
            .label(PLANNING)
            .schema(planner_schema()),
    );
    let plan = werk
        .finish_task(PLANNING)
        .await
        .unwrap_or_else(|| exit("the planner produced no result"));

    eprintln!("Researching the three angles…");
    for task in research_tasks(&question, &plan) {
        werk.add_task(task);
    }
    let findings = werk.finish_tasks(RESEARCH).await;
    if findings.len() != ANGLES {
        exit(&format!(
            "only {} of {ANGLES} research angles finished",
            findings.len()
        ));
    }

    eprintln!("Writing the report…");
    werk.add_task(
        Task::new(json!({
            "question": question,
            "findings": findings,
        }))
        .label(REPORT)
        .schema(report_schema()),
    );
    let report = werk
        .finish_task(REPORT)
        .await
        .unwrap_or_else(|| exit("the writer produced no report"));

    println!(
        "# {}\n\n{}",
        report["title"].as_str().unwrap_or("Research report"),
        report["report"].as_str().unwrap_or_default(),
    );
}

fn research_tasks(question: &str, plan: &Value) -> Vec<Task> {
    plan["angles"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|angle| {
            Task::new(json!({
                "question": question,
                "topic": angle["topic"],
                "query": angle["query"],
            }))
            .label(RESEARCH)
            .schema(research_schema())
        })
        .collect()
}

fn planner_schema() -> Schema {
    Schema::new(json!({
        "type": "object",
        "properties": {
            "angles": {
                "type": "array",
                "minItems": ANGLES,
                "maxItems": ANGLES,
                "items": {
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string", "minLength": 3 },
                        "query": { "type": "string", "minLength": 3 }
                    },
                    "required": ["topic", "query"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["angles"],
        "additionalProperties": false
    }))
    .expect("planner schema is valid")
}

fn research_schema() -> Schema {
    Schema::new(json!({
        "type": "object",
        "properties": {
            "topic": { "type": "string", "minLength": 3 },
            "summary": { "type": "string", "minLength": 100 },
            "sources": {
                "type": "array",
                "minItems": 2,
                "maxItems": 2,
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "minLength": 1 },
                        "url": { "type": "string", "pattern": "^https?://" }
                    },
                    "required": ["title", "url"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["topic", "summary", "sources"],
        "additionalProperties": false
    }))
    .expect("research schema is valid")
}

fn report_schema() -> Schema {
    Schema::new(json!({
        "type": "object",
        "properties": {
            "title": { "type": "string", "minLength": 1 },
            "report": { "type": "string", "minLength": 100 }
        },
        "required": ["title", "report"],
        "additionalProperties": false
    }))
    .expect("report schema is valid")
}

fn log_progress(event: &Event) {
    match event.get_name() {
        Event::TASK_STARTED => {
            eprintln!("  {} started {}", event.get_agent_id(), event.get_task_id())
        }
        Event::TASK_FINISHED => eprintln!(
            "  {} finished {}",
            event.get_agent_id(),
            event.get_task_id()
        ),
        Event::TOOL_CALL_STARTED => eprintln!(
            "  {} called {}",
            event.get_agent_id(),
            event.get_data()["tool_name"].as_str().unwrap_or("a tool")
        ),
        Event::TASK_FAILED => {
            eprintln!("  {} failed {}", event.get_agent_id(), event.get_task_id())
        }
        _ => {}
    }
}

fn cancel_on_ctrl_c(werk: Arc<Werk>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nCancelling research…");
            werk.cancel_all_tasks();
        }
    });
}

fn question_from_args() -> String {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let help = arguments
        .first()
        .is_some_and(|argument| matches!(argument.as_str(), "--help" | "-h"));
    if arguments.is_empty() || help {
        eprintln!("Usage: deep-research <QUESTION>");
        eprintln!();
        eprintln!("Environment:");
        eprintln!("  BRAVE_API_KEY       Required for web search");
        eprintln!("  ANTHROPIC_API_KEY   (or other provider env vars)");
        std::process::exit(if help { 0 } else { 1 });
    }
    arguments.join(" ")
}

fn exit(message: &str) -> ! {
    eprintln!("Error: {message}");
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(count: usize) -> Value {
        json!({
            "angles": (0..count)
                .map(|i| json!({ "topic": format!("Angle {i}"), "query": format!("Query {i}") }))
                .collect::<Vec<_>>()
        })
    }

    #[test]
    fn planner_requires_exactly_three_angles() {
        assert!(planner_schema().validate(plan(ANGLES)).is_ok());
        assert!(planner_schema().validate(plan(ANGLES - 1)).is_err());
        assert!(planner_schema().validate(plan(ANGLES + 1)).is_err());
    }

    #[test]
    fn each_planned_angle_becomes_a_research_task() {
        let tasks = research_tasks("Why?", &plan(ANGLES));

        assert_eq!(tasks.len(), ANGLES);
        assert!(tasks.iter().all(|task| task.get_label() == Some(RESEARCH)));
        assert!(tasks
            .iter()
            .all(|task| task.get_task()["question"] == "Why?"));
        assert!(tasks.iter().all(|task| task.get_schema().is_some()));
    }

    #[test]
    fn research_requires_two_sources() {
        let finding = |sources: Vec<Value>| {
            json!({
                "topic": "Maintenance",
                "summary": "A sufficiently long source-backed summary that explains the finding without relying on unsupported claims or vague language for the final report writer.",
                "sources": sources,
            })
        };
        let first = json!({ "title": "First", "url": "https://example.com/first" });
        let second = json!({ "title": "Second", "url": "https://example.com/second" });

        assert!(research_schema()
            .validate(finding(vec![first.clone(), second.clone()]))
            .is_ok());
        assert!(research_schema().validate(finding(vec![first])).is_err());
        assert!(research_schema()
            .validate(finding(vec![
                json!({ "title": "First", "url": "https://example.com/first" }),
                second,
                json!({ "title": "Third", "url": "https://example.com/third" }),
            ]))
            .is_err());
    }
}

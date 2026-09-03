//! Deep Research with result-routed stages.
//!
//! One `Werk` holds both research stages. The program enqueues a
//! single starter task pinned to `researcher_1`. Result hooks route each
//! completed research pass into the next stage, and the final task carries
//! both passes to the report writer.
//!
//! Usage: deep-research <QUESTION>
//!
//! Environment:
//!   BRAVE_API_KEY       Required for web search
//!   ANTHROPIC_API_KEY   (or other provider env vars)

use std::sync::Arc;

use agentwerk::event::Event;
use agentwerk::providers::{Model, Provider};
use agentwerk::schemas::Schema;
use agentwerk::tools::{FetchTool, Tool};
use agentwerk::{Agent, FinishReason, Task, Werk};

const RESEARCHER_1_ROLE: &str = include_str!("prompts/researcher-1.role.md");
const RESEARCHER_2_ROLE: &str = include_str!("prompts/researcher-2.role.md");
const REPORT_WRITER_ROLE: &str = include_str!("prompts/report-writer.role.md");

#[tokio::main]
async fn main() {
    let question = parse_question();
    let brave_key = check_required_env();

    eprintln!("Question: {question}\n");

    let provider = Provider::from_env().expect("LLM provider required");
    let event_handler: Arc<dyn Fn(&Event) + Send + Sync> =
        Arc::new(|event: &Event| log_event(event));

    let workdir = prepare_workdir();

    let werk = Werk::new();
    werk.set_dir(workdir.clone());
    let on_ctrl_c = Arc::clone(&werk);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.cancel_all_tasks();
        }
    });
    werk.on_event(move |_, e| event_handler(e));

    let report_schema =
        Schema::new(final_report_schema_value()).expect("report schema is well-formed");
    route_research_results(&werk, report_schema);

    let researcher_1 = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(RESEARCHER_1_ROLE)
        .label("researcher_1")
        .tool(brave_search_tool(brave_key.clone()))
        .tool(FetchTool::new().impersonate());

    let researcher_2 = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(RESEARCHER_2_ROLE)
        .label("researcher_2")
        .tool(brave_search_tool(brave_key.clone()))
        .tool(FetchTool::new().impersonate());

    let report_writer = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(REPORT_WRITER_ROLE)
        .label("report");

    werk.add_agent(researcher_1);
    werk.add_agent(researcher_2);
    werk.add_agent(report_writer);

    let starter = serde_json::json!({
        "question": question,
        "instruction": "Establish one angle with source-backed evidence."
    });
    // The schema-bound starter forces researcher_1 to produce a real
    // result: a text-only reply leaves none attached, and the loop's
    // terminal-reply path then transitions the task to `failed`
    // rather than silently `Done`. Result hooks keep the stages going.
    let starter_schema = Schema::new(serde_json::json!({
        "type": "string",
        "minLength": 100
    }))
    .expect("starter schema is well-formed");
    werk.add_task(
        Task::new(starter)
            .schema(starter_schema)
            .label("researcher_1"),
    );

    werk.finish_all_tasks().await;
    let outcome = classify_outcome(&werk);

    print_chain_summary(&werk);
    print_stats(&werk);
    print_research_outcome(&werk, &outcome);

    match outcome {
        Outcome::Report(_) => {}
        Outcome::Cancelled => std::process::exit(130),
        Outcome::Stalled => std::process::exit(1),
    }
}

fn route_research_results(werk: &Arc<Werk>, report_schema: Schema) {
    werk.on_result(move |werk, done, result| match done.get_label() {
        Some("researcher_1") => {
            werk.add_task(Task::labeled(
                "researcher_2",
                serde_json::json!({
                    "question": done.get_task()["question"].clone(),
                    "researcher_1": result,
                    "instruction": "Deepen and broaden these facts with causes, consequences, criticisms, or alternative perspectives."
                }),
            ));
        }
        Some("researcher_2") => {
            werk.add_task(
                Task::labeled(
                    "report",
                    serde_json::json!({
                        "question": done.get_task()["question"].clone(),
                        "researcher_1": done.get_task()["researcher_1"].clone(),
                        "researcher_2": result
                    }),
                )
                .schema(report_schema.clone()),
            );
        }
        _ => {}
    });
}

fn print_research_outcome(werk: &Werk, outcome: &Outcome) {
    eprintln!("\n══════════════════════════════════════════════════════════");
    match outcome {
        Outcome::Report(report) => {
            let title = report["title"].as_str().unwrap_or("(no title)");
            let research = report["research"].as_str().unwrap_or("(no body)");
            eprintln!(" REPORT");
            eprintln!("══════════════════════════════════════════════════════════\n");
            println!("## {title}\n\n{research}\n");
        }
        Outcome::Cancelled | Outcome::Stalled => {
            let label = match outcome {
                Outcome::Cancelled => "PARTIAL RESEARCH: cancelled before report writer finished",
                Outcome::Stalled => {
                    "PARTIAL RESEARCH: workflow stalled before report writer finished"
                }
                Outcome::Report(_) => unreachable!(),
            };
            eprintln!(" {label}");
            eprintln!("══════════════════════════════════════════════════════════\n");
            let researched: Vec<(String, String)> = werk
                .find_tasks("task.status = finished AND task.label != report")
                .iter()
                .filter_map(|t| Some((t.get_id().to_string(), plain_text(t.get_result()?))))
                .collect();
            if researched.is_empty() {
                eprintln!("(no researcher produced findings)");
            } else {
                for (id, findings) in researched {
                    println!("### {id}\n\n{findings}\n");
                }
            }
        }
    }
}

enum Outcome {
    Report(serde_json::Value),
    Cancelled,
    Stalled,
}

/// Read the run's outcome off the drained Werk: a finished report
/// task wins, an external cancel is surfaced, anything else means the
/// workflow stopped without reaching the report step.
fn classify_outcome(werk: &Werk) -> Outcome {
    let reported = werk.find_results("task.label = report").pop();
    if let Some(result) = reported {
        return Outcome::Report(result);
    }
    if werk.get_finish_reason() == Some(FinishReason::Cancelled) {
        return Outcome::Cancelled;
    }
    Outcome::Stalled
}

fn print_chain_summary(werk: &Werk) {
    eprintln!("\nTask summary:");
    let all = werk.get_tasks();
    if all.is_empty() {
        eprintln!("  (no tasks)");
        return;
    }
    for t in &all {
        let label = match t.get_label() {
            Some(l) => format!(" [{l}]"),
            None => String::new(),
        };
        let preview = t
            .get_result()
            .map(|v| truncate(&plain_text(v), 100))
            .unwrap_or_else(|| "(no result)".into());
        eprintln!(
            "  {id} {status}{label}\n      → {preview}",
            id = t.get_id(),
            status = t.get_status(),
        );
    }
}

fn print_stats(werk: &Werk) {
    eprintln!("\nStats:");
    eprintln!("  Duration : {:?}", werk.get_duration().unwrap_or_default());
    let count = |name: &str| werk.find_events(format!("event.name = {name}")).len() as u64;
    let done = count(Event::TASK_FINISHED);
    let failed = count(Event::TASK_FAILED);
    let resolved = done + failed;
    let success = if resolved == 0 {
        0.0
    } else {
        done as f64 / resolved as f64 * 100.0
    };
    eprintln!("  Tasks  : {done} done, {failed} failed ({success:.0}%)");
    eprintln!(
        "  Tokens   : {} in, {} out",
        werk.get_input_tokens(),
        werk.get_output_tokens(),
    );
    eprintln!(
        "  Activity : {} requests · {} tool calls · {} failed requests",
        count(Event::REQUEST_FINISHED),
        count(Event::TOOL_CALL_STARTED),
        count(Event::REQUEST_FAILED),
    );
}

fn prepare_workdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("agentwerk-deep-research");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create deep-research workdir");
    dir
}

fn final_report_schema_value() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "title":    { "type": "string", "minLength": 1 },
            "research": { "type": "string", "minLength": 1 }
        },
        "required": ["title", "research"],
        "additionalProperties": false
    })
}

fn brave_search_tool(api_key: String) -> Tool {
    Tool::new("brave_search")
        .description("Search the web. Returns titles, URLs, and descriptions.")
        .schema(serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "count": { "type": "integer", "description": "Results count (1-20, default: 5)" }
            },
            "required": ["query"]
        }))
        .concurrent(true)
        .handler(move |input: serde_json::Value| {
            let api_key = api_key.clone();
            async move { brave_search(&api_key, &input).await }
        })
}

async fn brave_search(api_key: &str, input: &serde_json::Value) -> Event {
    let query = input["query"].as_str().unwrap_or("").trim();
    let count = input["count"].as_u64().unwrap_or(5).min(20).to_string();

    let response = match reqwest::Client::new()
        .get("https://api.search.brave.com/res/v1/web/search")
        .query(&[("q", query), ("count", &count)])
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Event::new(Event::TOOL_CALL_FAILED).data(serde_json::json!({"kind": "execution_failed", "message": format!("Brave search failed: {e}")})),
    };

    let json: serde_json::Value = match response.json().await {
        Ok(j) => j,
        Err(e) => return Event::new(Event::TOOL_CALL_FAILED).data(serde_json::json!({"kind": "execution_failed", "message": format!("Failed to parse response: {e}")})),
    };

    let Some(results) = json["web"]["results"].as_array() else {
        return Event::new(Event::TOOL_CALL_FINISHED)
            .data(serde_json::json!({"output": "No results found."}));
    };

    let text = results
        .iter()
        .map(|r| {
            format!(
                "## {}\n{}\n{}\n",
                r["title"].as_str().unwrap_or(""),
                r["url"].as_str().unwrap_or(""),
                r["description"].as_str().unwrap_or(""),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    Event::new(Event::TOOL_CALL_FINISHED).data(serde_json::json!({"output": text}))
}

fn log_event(event: &Event) {
    let agent = event.get_agent_id();
    let id = event.get_task_id();
    let data = event.get_data();
    match event.get_name() {
        Event::TASK_STARTED => {
            eprintln!("\n┌─ [{agent}] picked up {id}");
        }
        Event::TOOL_CALL_STARTED => {
            let tool_name = data["tool_name"].as_str().unwrap_or_default();
            for line in format_tool_call(tool_name, &data["input"]) {
                eprintln!("│  {line}");
            }
        }
        Event::TOOL_CALL_FAILED => {
            eprintln!(
                "│  ✗ {} ({}): {}",
                data["tool_name"].as_str().unwrap_or_default(),
                data["kind"].as_str().unwrap_or_default(),
                data["message"].as_str().unwrap_or_default(),
            );
        }
        Event::SCHEMA_RETRIED => {
            eprintln!(
                "│  ↻ retry {}/{}: {}",
                data["attempt"],
                data["max_attempts"],
                truncate(data["message"].as_str().unwrap_or_default(), 110)
            );
        }
        Event::POLICY_VIOLATED => {
            eprintln!("│  ⚠ policy: {} limit={}", data["policy"], data["limit"]);
        }
        Event::TASK_FINISHED => {
            eprintln!("└─ ✓ finished {id}");
        }
        Event::TASK_FAILED => {
            eprintln!("└─ ✗ failed {id}");
        }
        _ => {}
    }
}

fn format_tool_call(tool_name: &str, input: &serde_json::Value) -> Vec<String> {
    match tool_name {
        "brave_search" => vec![format!(
            "🔎 search: {}",
            truncate(input["query"].as_str().unwrap_or(""), 70),
        )],
        "task" => {
            let action = input["action"].as_str().unwrap_or("?");
            let id = input.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let suffix = if id.is_empty() {
                String::new()
            } else {
                format!(" {id}")
            };
            vec![format!("📖 read tasks {action}{suffix}")]
        }
        "finish" => {
            let result = preview_value(input.get("result"), 80);
            vec![format!("✅ final result: {result}")]
        }
        _ => vec![format!(
            "{tool_name}: {}",
            serde_json::to_string(input).unwrap_or_default()
        )],
    }
}

fn preview_value(value: Option<&serde_json::Value>, max: usize) -> String {
    truncate(&value.map(plain_text).unwrap_or_default(), max)
}

/// A result as prose: a JSON string loses its quotes, anything else keeps
/// its JSON form.
fn plain_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let one_line: String = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max).collect();
    format!("{cut}…")
}

fn parse_question() -> String {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args[1] == "--help" || args[1] == "-h" {
        eprintln!("Usage: deep-research <QUESTION>");
        eprintln!();
        eprintln!("Example: deep-research \"Should we use Rust or Go for our backend?\"");
        eprintln!();
        eprintln!("Environment:");
        eprintln!("  BRAVE_API_KEY       Required for web search");
        eprintln!("  ANTHROPIC_API_KEY   (or other provider env vars)");
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    args[1..].join(" ")
}

fn check_required_env() -> String {
    let brave_key = std::env::var("BRAVE_API_KEY").unwrap_or_default();
    if brave_key.is_empty() {
        eprintln!("Error: missing environment variable: BRAVE_API_KEY");
        std::process::exit(1);
    }
    brave_key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_router_files_each_remaining_stage_once() {
        let werk = Werk::new();
        let report_schema = Schema::new(final_report_schema_value()).unwrap();
        route_research_results(&werk, report_schema);
        let first = werk.add_task(Task::labeled(
            "researcher_1",
            serde_json::json!({"question": "Why?"}),
        ));

        werk.set_task_finished(&first, "first findings").unwrap();
        let second = werk.find_task("task.label = researcher_2").unwrap();
        assert_eq!(second.get_task()["researcher_1"], "first findings");

        werk.set_task_finished(second.get_id(), "second findings")
            .unwrap();
        let reports = werk.find_tasks("task.label = report");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].get_task()["researcher_1"], "first findings");
        assert_eq!(reports[0].get_task()["researcher_2"], "second findings");
        assert!(reports[0].get_schema().is_some());
    }
}

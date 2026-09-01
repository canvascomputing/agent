//! Deep Research with handover chain.
//!
//! One `Queue` holds the whole pipeline. The driver enqueues a
//! single starter task pinned to `researcher_1`. Each researcher calls
//! `brave_search` and hands off to the next agent through its configured
//! handover. The report contract is attached to the task researcher_2 creates.
//! The report writer reads that research chain and finishes it with a plain
//! `finish`.
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
use agentwerk::tools::{FetchTool, TaskTool, Tool};
use agentwerk::{Agent, FinishReason, Queue, Task};

const RESEARCHER_1_ROLE: &str = include_str!("prompts/researcher_1.role.md");
const RESEARCHER_2_ROLE: &str = include_str!("prompts/researcher_2.role.md");
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

    let tasks = Queue::new();
    tasks.set_dir(workdir.clone());
    let on_ctrl_c = Arc::clone(&tasks);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.cancel_all_tasks();
        }
    });
    tasks.on_event(move |_, e| event_handler(e));

    let researcher_1 = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(RESEARCHER_1_ROLE)
        .label("researcher_1")
        .handover(Task::labeled(
            "researcher_2",
            "Researcher 1 task: {parent_id}\n\nResearcher 1 findings:\n{parent_result}\n\nDeepen and broaden these facts with causes, consequences, criticisms, or alternative perspectives.",
        ))
        .tool(brave_search_tool(brave_key.clone()))
        .tool(FetchTool::new().impersonate());

    let researcher_2 = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(RESEARCHER_2_ROLE)
        .label("researcher_2")
        .handover(
            Task::labeled(
                "report",
                "Synthesize both research passes into a structured final report.\n\nResearcher 2 task: {parent_id}\n\nResearcher 2 findings:\n{parent_result}",
            )
            .schema(
                Schema::new(final_report_schema_value())
                    .expect("report schema is well-formed"),
            ),
        )
        .tool(brave_search_tool(brave_key.clone()))
        .tool(FetchTool::new().impersonate());

    let report_writer = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(REPORT_WRITER_ROLE)
        .label("report")
        .tool(TaskTool);

    tasks.add_agent(researcher_1);
    tasks.add_agent(researcher_2);
    tasks.add_agent(report_writer);

    let starter =
        format!("Question: {question}\n\nEstablish one angle with source-backed evidence.");
    // The schema-bound starter forces researcher_1 to produce a real
    // result: a text-only reply leaves none attached, and the loop's
    // terminal-reply path then transitions the task to `Failed`
    // rather than silently `Done`. Configured handovers keep the chain going.
    let starter_schema = Schema::new(serde_json::json!({
        "type": "string",
        "minLength": 100
    }))
    .expect("starter schema is well-formed");
    tasks.add_task(
        Task::new(starter)
            .schema(starter_schema)
            .label("researcher_1"),
    );

    tasks.finish_all_tasks().await;
    let outcome = classify_outcome(&tasks);

    print_chain_summary(&tasks);
    print_stats(&tasks);
    print_research_outcome(&tasks, &outcome);

    match outcome {
        Outcome::Report(_) => {}
        Outcome::Cancelled => std::process::exit(130),
        Outcome::Stalled => std::process::exit(1),
    }
}

fn print_research_outcome(tasks: &Queue, outcome: &Outcome) {
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
                Outcome::Stalled => "PARTIAL RESEARCH: chain stalled before report writer finished",
                Outcome::Report(_) => unreachable!(),
            };
            eprintln!(" {label}");
            eprintln!("══════════════════════════════════════════════════════════\n");
            let researched: Vec<(String, String)> = tasks
                .find_tasks("status = Finished AND label != report")
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

/// Read the run's outcome off the drained queue: a finished report
/// task wins, an external cancel is surfaced, anything else means the
/// chain stopped without reaching the report step.
fn classify_outcome(tasks: &Queue) -> Outcome {
    let reported = tasks.find_results("report").pop();
    if let Some(result) = reported {
        return Outcome::Report(result);
    }
    if tasks.get_finish_reason() == Some(FinishReason::Cancelled) {
        return Outcome::Cancelled;
    }
    Outcome::Stalled
}

fn print_chain_summary(tasks: &Queue) {
    eprintln!("\nChain summary:");
    let all = tasks.get_tasks();
    if all.is_empty() {
        eprintln!("  (no tasks)");
        return;
    }
    for t in &all {
        let parent = t
            .get_parent()
            .map(|p| format!(" ⟵ {p}"))
            .unwrap_or_default();
        let label = match t.get_label() {
            Some(l) => format!(" [{l}]"),
            None => String::new(),
        };
        let preview = t
            .get_result()
            .map(|v| truncate(&plain_text(v), 100))
            .unwrap_or_else(|| "(no result)".into());
        eprintln!(
            "  {id} {status}{label}{parent}\n      → {preview}",
            id = t.get_id(),
            status = t.get_status(),
        );
    }
}

fn print_stats(tasks: &Queue) {
    eprintln!("\nStats:");
    eprintln!(
        "  Duration : {:?}",
        tasks.get_duration().unwrap_or_default()
    );
    let count = |name: &str| tasks.find_events(name).len() as u64;
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
        tasks.get_input_tokens(),
        tasks.get_output_tokens(),
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
        "tasks" => {
            let action = input["action"].as_str().unwrap_or("?");
            let id = input.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let suffix = if id.is_empty() {
                String::new()
            } else {
                format!(" {id}")
            };
            vec![format!("📖 read tasks {action}{suffix}")]
        }
        // A `handover` in the arguments is what tells a chaining finish
        // apart from the terminal one that ends the run.
        "finish" => match input.get("handover").and_then(|v| v.as_str()) {
            Some(to) => {
                let task = preview_value(input.get("task"), 70);
                let result = preview_value(input.get("result"), 70);
                vec![
                    format!("📤 handoff → {to}"),
                    format!("      · task    : {task}"),
                    format!("      · findings: {result}"),
                ]
            }
            None => {
                let result = preview_value(input.get("result"), 80);
                vec![format!("✅ final result: {result}")]
            }
        },
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

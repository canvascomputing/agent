//! Deep Research with handover chain.
//!
//! One `TicketQueue` holds the whole pipeline. The driver enqueues a
//! single starter ticket pinned to `researcher_1`. Each researcher
//! calls `brave_search`, reads its parent ticket via
//! `tickets` to build on prior findings, and hands off to
//! the next agent via `finish` with a `handover`. A handover carries no
//! schema, so the report schema is bound to the `report` label and the
//! report writer's ticket takes it when that agent claims it. The report
//! writer finishes the chain with a plain `finish`.
//!
//! Usage: deep-research <QUESTION>
//!
//! Environment:
//!   BRAVE_API_KEY       Required for web search
//!   ANTHROPIC_API_KEY   (or other provider env vars)

use std::sync::Arc;

use agentwerk::event::{Event, EventKind, EventName};
use agentwerk::providers::{Model, Provider};
use agentwerk::schemas::{Schema, SchemaStore};
use agentwerk::tools::{FetchUrlTool, TicketsTool, Tool, ToolResult};
use agentwerk::{Agent, FinishReason, Ticket, TicketQueue};

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

    let tickets = TicketQueue::new();
    tickets.dir(workdir.clone());
    let schemas = SchemaStore::new();
    schemas
        .label("report", final_report_schema_value())
        .expect("report schema is well-formed");
    tickets.schemas(&schemas);
    let on_ctrl_c = Arc::clone(&tickets);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.cancel_all();
        }
    });
    tickets.on_event(move |_, e| event_handler(e));

    let researcher_1 = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(RESEARCHER_1_ROLE)
        .label("researcher_1")
        .tool(brave_search_tool(brave_key.clone()))
        .tool(FetchUrlTool::new().impersonate())
        .tool(TicketsTool);

    let researcher_2 = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(RESEARCHER_2_ROLE)
        .label("researcher_2")
        .tool(brave_search_tool(brave_key.clone()))
        .tool(FetchUrlTool::new().impersonate())
        .tool(TicketsTool);

    let report_writer = Agent::new()
        .provider(provider.clone())
        .model(Model::from_env().expect("model name required"))
        .role(REPORT_WRITER_ROLE)
        .label("report")
        .tool(TicketsTool);

    tickets.agent(researcher_1);
    tickets.agent(researcher_2);
    tickets.agent(report_writer);

    let starter = format!(
        "Question: {question}\n\nKick off the research chain. You are researcher_1; pick \
         one angle and produce evidence with sources. The next two researchers will \
         extend the coverage."
    );
    // The schema-bound starter forces researcher_1 to produce a real
    // result: a text-only reply leaves none attached, and the loop's
    // terminal-reply path then transitions the ticket to `Failed`
    // rather than silently `Done`. The role prompt is what keeps the
    // chain going by requiring a `handover`.
    let starter_schema = Schema::new(serde_json::json!({
        "type": "string",
        "minLength": 100
    }))
    .expect("starter schema is well-formed");
    tickets.ticket(
        Ticket::new(starter)
            .schema(starter_schema)
            .label("researcher_1"),
    );

    tickets.finish_all().await;
    let outcome = classify_outcome(&tickets);

    print_chain_summary(&tickets);
    print_stats(&tickets);
    print_research_outcome(&tickets, &outcome);

    match outcome {
        Outcome::Report(_) => {}
        Outcome::Cancelled => std::process::exit(130),
        Outcome::Stalled => std::process::exit(1),
    }
}

fn print_research_outcome(tickets: &TicketQueue, outcome: &Outcome) {
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
            let researched: Vec<(String, String)> = tickets
                .find_tickets(|t: &Ticket| t.is_finished() && !t.has_label("report"))
                .iter()
                .filter_map(|t| Some((t.key.clone(), plain_text(t.result.as_ref()?))))
                .collect();
            if researched.is_empty() {
                eprintln!("(no researcher produced findings)");
            } else {
                for (key, findings) in researched {
                    println!("### {key}\n\n{findings}\n");
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
/// ticket wins, an external cancel is surfaced, anything else means the
/// chain stopped without reaching the report step.
fn classify_outcome(tickets: &TicketQueue) -> Outcome {
    let reported = tickets.find_results("report").pop();
    if let Some(result) = reported {
        return Outcome::Report(result);
    }
    if tickets.finish_reason() == Some(FinishReason::Cancelled) {
        return Outcome::Cancelled;
    }
    Outcome::Stalled
}

fn print_chain_summary(tickets: &TicketQueue) {
    eprintln!("\nChain summary:");
    let all = tickets.tickets();
    if all.is_empty() {
        eprintln!("  (no tickets)");
        return;
    }
    for t in &all {
        let parent = t
            .parent
            .as_deref()
            .map(|p| format!(" ⟵ {p}"))
            .unwrap_or_default();
        let label = match t.label.as_deref() {
            Some(l) => format!(" [{l}]"),
            None => String::new(),
        };
        let preview = t
            .result
            .as_ref()
            .map(|v| truncate(&plain_text(v), 100))
            .unwrap_or_else(|| "(no result)".into());
        eprintln!(
            "  {key} {status}{label}{parent}\n      → {preview}",
            key = t.key,
            status = t.status,
        );
    }
}

fn print_stats(tickets: &TicketQueue) {
    eprintln!("\nStats:");
    eprintln!(
        "  Duration : {:?}",
        tickets.execution_duration().unwrap_or_default()
    );
    let count = |kind: EventName| tickets.find_events(kind.name()).len() as u64;
    let done = count(EventName::TicketFinished);
    let failed = count(EventName::TicketFailed);
    let resolved = done + failed;
    let success = if resolved == 0 {
        0.0
    } else {
        done as f64 / resolved as f64 * 100.0
    };
    eprintln!("  Tickets  : {done} done, {failed} failed ({success:.0}%)");
    eprintln!(
        "  Tokens   : {} in, {} out",
        tickets.input_tokens(),
        tickets.output_tokens(),
    );
    eprintln!(
        "  Activity : {} requests · {} tool calls · {} failed requests",
        count(EventName::RequestFinished),
        count(EventName::ToolCallStarted),
        count(EventName::RequestFailed),
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
        .handler(move |input: serde_json::Value, _ctx| {
            let api_key = api_key.clone();
            async move { brave_search(&api_key, &input).await }
        })
        .build()
}

async fn brave_search(api_key: &str, input: &serde_json::Value) -> ToolResult {
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
        Err(e) => return ToolResult::error(format!("Brave search failed: {e}")),
    };

    let json: serde_json::Value = match response.json().await {
        Ok(j) => j,
        Err(e) => return ToolResult::error(format!("Failed to parse response: {e}")),
    };

    let Some(results) = json["web"]["results"].as_array() else {
        return ToolResult::success("No results found.");
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

    ToolResult::success(text)
}

fn log_event(event: &Event) {
    let agent = &event.agent_id;
    let key = &event.ticket_key;
    match &event.kind {
        EventKind::TicketStarted => {
            eprintln!("\n┌─ [{agent}] picked up {key}");
        }
        EventKind::ToolCallStarted {
            tool_name, input, ..
        } => {
            for line in format_tool_call(tool_name, input) {
                eprintln!("│  {line}");
            }
        }
        EventKind::ToolCallFailed {
            tool_name,
            message,
            reason,
            ..
        } => {
            eprintln!("│  ✗ {tool_name} ({reason:?}): {message}");
        }
        EventKind::SchemaRetried {
            attempt,
            max_attempts,
            message,
        } => {
            eprintln!(
                "│  ↻ retry {attempt}/{max_attempts}: {}",
                truncate(message, 110)
            );
        }
        EventKind::PolicyViolated { policy, limit } => {
            eprintln!("│  ⚠ policy: {policy:?} limit={limit}");
        }
        EventKind::TicketFinished => {
            eprintln!("└─ ✓ finished {key}");
        }
        EventKind::TicketFailed => {
            eprintln!("└─ ✗ failed {key}");
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
        "tickets" => {
            let action = input["action"].as_str().unwrap_or("?");
            let key = input.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let suffix = if key.is_empty() {
                String::new()
            } else {
                format!(" {key}")
            };
            vec![format!("📖 read tickets {action}{suffix}")]
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

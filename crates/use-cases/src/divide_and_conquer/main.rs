//! Divide-and-conquer sum of squares.
//!
//! Partitions `[1, N]` into K subranges and creates one task per
//! subrange. Agents share the labelled Werk, call the `python` tool
//! for an exact integer, and finish via `finish` with a
//! schema-validated `{"idx", "partial_sum"}`. The driver aggregates
//! after `finish` returns and verifies the total against the
//! closed-form `N(N+1)(2N+1)/6`.
//!
//! Usage: divide-and-conquer [OPTIONS] [N]
//!
//! Example:
//!   divide-and-conquer 10000                # default: 16 partitions, 8 agents
//!   divide-and-conquer -p 32 -c 16 100000

use std::io::IsTerminal;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use agentwerk::event::Event;
use agentwerk::providers::{Model, Provider};
use agentwerk::schemas::Schema;
use agentwerk::tools::{TaskTool, Tool};
use agentwerk::{Agent, Policy, Task, Werk};
use serde_json::{json, Value};

const ROLE: &str = include_str!("prompts/agent.role.md");

#[tokio::main]
async fn main() {
    let args = CliArgs::parse();
    let provider = Provider::from_env().expect("LLM provider required");
    let model = Model::from_env().expect("model name required");
    let style = Style::detect();

    let partitions = partition(args.n, args.partitions);
    let agents = args.concurrency.min(partitions.len());
    print_intro(args.n, partitions.len(), agents, &style);

    let schema = partial_sum_schema();
    let werk = Werk::new();
    let on_ctrl_c = Arc::clone(&werk);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.cancel_all_tasks();
        }
    });
    werk.set_policy(Policy {
        max_turns: args.max_turns,
        ..Default::default()
    });

    for (idx, (lo, hi)) in partitions.iter().enumerate() {
        let body = format!(
            "Compute the partial sum S = sum_{{k={lo}}}^{{{hi}}} k^2.\n\
             lo={lo}\nhi={hi}\nidx={idx}",
        );
        werk.add_task(Task::new(body).schema(schema.clone()).label("compute"));
    }

    let event_handler = build_event_handler(args.verbose, style.clone(), partitions.len());
    werk.on_event(move |_, e| event_handler(e));
    for _ in 0..agents {
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model(&model)
                .role(ROLE)
                .label("compute")
                .tool(python_tool())
                .tool(TaskTool),
        );
    }

    werk.finish_all_tasks().await;

    aggregate_and_report(&werk, &partitions, args.n, &style);
}

fn aggregate_and_report(werk: &Werk, partitions: &[(u64, u64)], n: u64, style: &Style) {
    let total = partitions.len();
    let mut partials: Vec<Option<i128>> = vec![None; total];
    let mut failures = 0usize;

    for task in werk.get_tasks() {
        match extract_partial(&task, total) {
            Ok((idx, sum)) => {
                let (lo, hi) = partitions[idx];
                eprintln!(
                    "{dim}│{reset} chunk_{idx:<3}  {lo:>9}..{hi:<9}  {green}={reset} {sum:>20}",
                    dim = style.dim,
                    green = style.green,
                    reset = style.reset,
                );
                partials[idx] = Some(sum);
            }
            Err(reason) => {
                failures += 1;
                eprintln!(
                    "{red}│{reset} {id:<8} ✗ {reason}",
                    id = task.get_id(),
                    red = style.red,
                    reset = style.reset,
                );
            }
        }
    }

    let total_sum: i128 = partials.iter().flatten().sum();
    let expected = closed_form(n);
    let elapsed = werk.get_duration().unwrap_or_default().as_secs_f64();
    let done = werk
        .find_events(|event: &Event| event.get_name() == Event::TASK_FINISHED)
        .len();

    eprintln!(
        "{dim}└ aggregated in {elapsed:.1}s · {done} done, {failures} failed · {} in / {} out tokens{reset}",
        werk.get_input_tokens(),
        werk.get_output_tokens(),
        dim = style.dim,
        reset = style.reset,
    );
    println!();
    println!("aggregated sum : {total_sum}");
    println!("closed form    : {expected}");

    if failures > 0 {
        println!(
            "{red}✗{reset} {failures} partition(s) failed: aggregate incomplete",
            red = style.red,
            reset = style.reset,
        );
        std::process::exit(1);
    }
    if total_sum != expected {
        println!(
            "{red}✗{reset} mismatch: off by {}",
            total_sum - expected,
            red = style.red,
            reset = style.reset,
        );
        std::process::exit(1);
    }
    println!(
        "{green}✓ verified{reset}",
        green = style.green,
        reset = style.reset,
    );
}

/// Pull a `(idx, partial_sum)` pair off a finished task. The schema
/// already guarantees the field shape; this also cross-checks `idx`
/// against the `idx=` line in the task body so a wrongly assigned result
/// can't quietly slot into the wrong partition.
fn extract_partial(task: &Task, total: usize) -> Result<(usize, i128), String> {
    if !task.is_finished() {
        return Err(task.get_status().to_string());
    }
    let attached = task.get_result().ok_or("no result attached")?;
    let idx = attached
        .get("idx")
        .and_then(|v| v.as_u64())
        .ok_or("idx missing")? as usize;
    let sum = attached
        .get("partial_sum")
        .and_then(|v| v.as_i64())
        .ok_or("partial_sum missing")? as i128;

    if idx >= total {
        return Err(format!("idx {idx} out of range"));
    }
    let body_idx = parse_idx_from_body(task.get_task());
    if body_idx != Some(idx) {
        return Err(format!("idx mismatch: body={body_idx:?}, result={idx}"));
    }
    Ok((idx, sum))
}

fn parse_idx_from_body(task: &Value) -> Option<usize> {
    task.as_str()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("idx=")))
        .and_then(|n| n.trim().parse().ok())
}

fn partial_sum_schema() -> Schema {
    Schema::new(json!({
        "type": "object",
        "properties": {
            "idx": {
                "type": "integer",
                "description": "Partition index, copied verbatim from the task"
            },
            "partial_sum": {
                "type": "integer",
                "description": "Exact integer value of the partial sum"
            }
        },
        "required": ["idx", "partial_sum"],
        "additionalProperties": false
    }))
    .expect("partial-sum schema is well-formed")
}

fn python_tool() -> Tool {
    Tool::new("python")
        .description(
            "Run a short Python 3 snippet. The `code` field is passed directly to \
             `python3 -c`. Return value is the snippet's stdout, trimmed. Use this \
             for exact integer arithmetic.",
        )
        .schema(json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Python 3 source. Must print the result to stdout."
                }
            },
            "required": ["code"]
        }))
        .concurrent(true)
        .handler(|input: serde_json::Value| async move {
            let code = input
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if code.is_empty() {
                return Event::tool_call_failed("missing required field `code`");
            }

            let output_fut = tokio::process::Command::new("python3")
                .arg("-c")
                .arg(code)
                .kill_on_drop(true)
                .output();

            match output_fut.await {
                Err(error) => Event::tool_call_failed(format!("failed to spawn python3: {error}")),
                Ok(output) if output.status.success() => {
                    Event::tool_call_finished(String::from_utf8_lossy(&output.stdout).trim())
                }
                Ok(output) => Event::tool_call_failed(format!(
                    "python error: {}",
                    String::from_utf8_lossy(&output.stderr)
                )),
            }
        })
}

fn build_event_handler(
    verbose: bool,
    style: Style,
    total: usize,
) -> Arc<dyn Fn(&Event) + Send + Sync> {
    let done = Arc::new(AtomicUsize::new(0));
    let width = digit_width(total);
    Arc::new(move |event: &Event| {
        let agent = event.get_agent_id();
        let id = event.get_task_id();
        let data = event.get_data();
        match event.get_name() {
            Event::TASK_STARTED => eprintln!(
                "{dim}│       ▶ {agent:<10} {id} dispatched{reset}",
                dim = style.dim,
                reset = style.reset,
            ),
            Event::TASK_FINISHED | Event::TASK_FAILED => {
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                let outcome = if event.get_name() == Event::TASK_FINISHED {
                    "done"
                } else {
                    "failed"
                };
                eprintln!(
                    "{dim}│ {n:>width$}/{total} ▾ {agent:<10} {id} {outcome}{reset}",
                    dim = style.dim,
                    reset = style.reset,
                );
            }
            Event::TOOL_CALL_STARTED if verbose => {
                let tool_name = data["tool_name"].as_str().unwrap_or_default();
                let snippet = data["input"]
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                eprintln!(
                    "{dim}│    {agent} → {tool_name}({}){reset}",
                    truncate(snippet, 70),
                    dim = style.dim,
                    reset = style.reset,
                );
            }
            Event::TOOL_CALL_FAILED => eprintln!(
                "{red}│    {agent} ✗ {tool_name}: {}{reset}",
                truncate(data["message"].as_str().unwrap_or_default(), 120),
                tool_name = data["tool_name"].as_str().unwrap_or_default(),
                red = style.red,
                reset = style.reset,
            ),
            Event::REQUEST_FAILED => eprintln!(
                "{red}│    {agent} ✗ request failed: {}{reset}",
                truncate(data["message"].as_str().unwrap_or_default(), 120),
                red = style.red,
                reset = style.reset,
            ),
            Event::POLICY_VIOLATED => eprintln!(
                "{red}│    {agent} ✗ policy {policy:?} (limit {limit}){reset}",
                policy = data["policy"],
                limit = data["limit"],
                red = style.red,
                reset = style.reset,
            ),
            _ => {}
        }
    })
}

fn print_intro(n: u64, partitions: usize, agents: usize, style: &Style) {
    eprintln!("divide-and-conquer   sum_{{k=1}}^{{{n}}} k^2   (verified via N(N+1)(2N+1)/6)\n");
    eprintln!("  Split [1, {n}] into {partitions} contiguous subranges and enqueue one task per");
    eprintln!("  subrange. {agents} agent(s) share the Werk, each calling a `python` tool");
    eprintln!("  to compute its partial sum exactly. Agents finish their tasks via");
    eprintln!("  `finish` with `{{\"idx\", \"partial_sum\"}}`; the driver aggregates");
    eprintln!("  once every task is finished and verifies against the closed-form total.\n");
    eprintln!(
        "{dim}┌ {partitions} partitions · {agents} agent(s) sharing the Werk{reset}",
        dim = style.dim,
        reset = style.reset,
    );
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn digit_width(n: usize) -> usize {
    let mut n = n.max(1);
    let mut w = 0;
    while n > 0 {
        n /= 10;
        w += 1;
    }
    w
}

fn partition(n: u64, k: usize) -> Vec<(u64, u64)> {
    let k = k.max(1).min(n.max(1) as usize);
    let base = n / k as u64;
    let extra = n % k as u64;
    let mut out = Vec::with_capacity(k);
    let mut lo = 1u64;
    for i in 0..k {
        let size = base + if (i as u64) < extra { 1 } else { 0 };
        let hi = lo + size - 1;
        out.push((lo, hi));
        lo = hi + 1;
    }
    out
}

fn closed_form(n: u64) -> i128 {
    let n = i128::from(n);
    n * (n + 1) * (2 * n + 1) / 6
}

#[derive(Clone)]
struct Style {
    dim: &'static str,
    green: &'static str,
    red: &'static str,
    reset: &'static str,
}

impl Style {
    fn detect() -> Self {
        if std::io::stderr().is_terminal() {
            Self {
                dim: "\x1b[2m",
                green: "\x1b[32m",
                red: "\x1b[31m",
                reset: "\x1b[0m",
            }
        } else {
            Self {
                dim: "",
                green: "",
                red: "",
                reset: "",
            }
        }
    }
}

struct CliArgs {
    n: u64,
    partitions: usize,
    concurrency: usize,
    max_turns: Option<u32>,
    verbose: bool,
}

impl CliArgs {
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut n: Option<u64> = None;
        let mut partitions: usize = 16;
        let mut concurrency: usize = 8;
        let mut max_turns: Option<u32> = None;
        let mut verbose = false;

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "-p" | "--partitions" => {
                    i += 1;
                    partitions = parse_value(args.get(i), "--partitions");
                }
                "-c" | "--concurrency" => {
                    i += 1;
                    concurrency = parse_value(args.get(i), "--concurrency");
                }
                "--max-turns" => {
                    i += 1;
                    max_turns = Some(parse_value(args.get(i), "--max-turns"));
                }
                "-v" | "--verbose" => verbose = true,
                "-h" | "--help" => {
                    Self::print_help();
                    std::process::exit(0);
                }
                arg if arg.starts_with('-') => bad_arg(&format!("unknown flag: {arg}")),
                _ => {
                    n = Some(
                        args[i]
                            .parse()
                            .unwrap_or_else(|_| bad_arg("N must be a positive integer")),
                    );
                }
            }
            i += 1;
        }

        Self {
            n: n.unwrap_or(10_000),
            partitions,
            concurrency,
            max_turns,
            verbose,
        }
    }

    fn print_help() {
        eprintln!("Divide-and-conquer sum of squares.\n");
        eprintln!("Usage: divide-and-conquer [OPTIONS] [N]\n");
        eprintln!("Options:");
        eprintln!("  -p, --partitions <K>   Number of task partitions (default: 16)");
        eprintln!("  -c, --concurrency <N>  Number of agents sharing the Werk (default: 8)");
        eprintln!("      --max-turns <N>    per-Werk turn limit (default: unlimited)");
        eprintln!("  -v, --verbose          Print per-agent tool calls as they happen");
        eprintln!("  -h, --help             Show this help\n");
        eprintln!("Examples:");
        eprintln!("  divide-and-conquer 10000");
        eprintln!("  divide-and-conquer -p 32 -c 16 100000");
    }
}

fn parse_value<T: std::str::FromStr>(value: Option<&String>, flag: &str) -> T {
    value
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| bad_arg(&format!("{flag} expects a positive number")))
}

fn bad_arg(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

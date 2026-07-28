//! Interactive terminal chat. One `TicketSystem` + `Agent` + `Knowledge`
//! lives for the whole session, and one chat ticket spans every turn:
//! the first input creates the ticket via `tickets.task(...)`, every
//! subsequent input lands as a user reply via `tickets.reply(&key, ...)`.
//! The agent loop's wait-for-input branch picks each comment up and
//! drives the next model turn on the same growing transcript. Tickets
//! and knowledge both persist to `./.agentwerk/`, so an existing chat
//! resumes across process restarts.
//! The model's response streams to stdout via
//! `EventKind::TextChunkReceived`. Slash commands:
//! `/new` starts a fresh chat ticket, `/list` lists every ticket,
//! `/stats` prints run counters, `/clear` resets knowledge,
//! `/bible [N]` injects N repetitions of Genesis (KJV) as a reply to
//! drive context compaction (default N=1, ~52k tokens per repetition).
//! `/scrub <word>` redacts that word from the transcript in place (via
//! `edit_replies`) with no model turn; the word is gone on disk too.
//! Ctrl-C at the prompt exits with code 130; Ctrl-D exits with
//! code 0; Ctrl-C during a turn cancels that turn (a second Ctrl-C
//! while the cancel is still draining force-quits with exit code 130).
//!
//! Every exit path goes through `std::process::exit` rather than a
//! plain `return`: the stdin reader runs on a tokio blocking thread
//! parked in `read(2)`, which the runtime can't cancel on shutdown.
//! Exiting the process directly bypasses the runtime drop and avoids
//! a hang on outstanding blocking tasks.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use agentwerk::agents::tickets::{Author, Reply, ReplyContent};
use agentwerk::event::{Event, EventKind};
use agentwerk::providers::{context_window_from_env, model_from_env, Model};
use agentwerk::tools::{
    GlobTool, GrepTool, ListDirectoryTool, ManageTicketsTool, ReadFileTool, ReadTicketsTool,
    WriteFileTool,
};
use agentwerk::{Agent, Knowledge, Ticket, TicketSystem};

const ROLE: &str = include_str!("prompts/repl.role.md");
const BIBLE_PASSAGE: &str = include_str!("prompts/bible.txt");
const BIBLE_DEFAULT_REPETITIONS: usize = 1;

#[tokio::main]
async fn main() {
    let style = Style::detect();
    eprintln!(
        "{}agentwerk REPL: /new /list /stats /clear /bible /scrub, Ctrl-C to cancel.{}",
        style.dim, style.reset,
    );

    // Optional first positional arg overrides the model's real context
    // window for the REPL's own usage line. The library's compaction
    // thresholds still derive from the model itself; this knob only
    // changes what the REPL prints.
    let test_window: Option<u64> = std::env::args().nth(1).and_then(|s| s.parse().ok());
    let real_window = context_window_from_env().or_else(|| {
        model_from_env()
            .ok()
            .and_then(|name| Model::from_name(&name).get_context_window())
    });
    let effective_window = test_window.or(real_window);
    match (test_window, real_window) {
        (Some(w), _) => {
            let threshold = w.saturating_mul(7) / 10;
            eprintln!(
                "{}test context window: {w} tokens (warn at {threshold}){}",
                style.dim, style.reset,
            );
        }
        (None, Some(w)) => eprintln!("{}context window: {w} tokens{}", style.dim, style.reset),
        (None, None) => {}
    }

    let role = ROLE.trim();

    let user_prompt = format!("\n{}you ›{} ", style.user, style.reset);

    let event_style = style.clone();
    // `midstream` tracks whether the last byte written was streamed
    // model text (no trailing newline). Stderr event lines consult it
    // to break out of the stream exactly once, instead of every
    // `eprintln!` doubling up newlines.
    let midstream = Arc::new(AtomicBool::new(false));
    let handler_midstream = Arc::clone(&midstream);
    let last_input = Arc::new(AtomicU64::new(0));
    let handler_last_input = Arc::clone(&last_input);
    let handler: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(move |e: Event| {
        print_event(
            &e,
            &event_style,
            effective_window,
            &handler_midstream,
            &handler_last_input,
        )
    });

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let store_dir = cwd.join(".agentwerk");
    let tickets = TicketSystem::load(&store_dir).expect("open ticket store");
    tickets.max_turns(40);

    let knowledge = Knowledge::load(&store_dir).expect("open knowledge store");

    tickets.on_event(move |e| handler(e));
    let _agent = tickets.agent(
        Agent::new()
            .name("orchestrator")
            .interactive()
            .from_env()
            .role(role)
            .dir(&cwd)
            .tool(GlobTool)
            .tool(GrepTool)
            .tool(ListDirectoryTool)
            .tool(ReadFileTool)
            .tool(WriteFileTool)
            .tool(ReadTicketsTool)
            .tool(ManageTicketsTool)
            .knowledge(&knowledge)
            .build(),
    );

    let mut prev_turns: u64 = 0;
    let mut prev_requests: u64 = 0;
    let mut prev_tool_calls: u64 = 0;
    let mut prev_input: u64 = 0;
    let mut prev_output: u64 = 0;

    let failed = fail_stale_chats(&tickets, "orchestrator");
    if failed > 0 {
        eprintln!(
            "{}failed {} stale chat ticket{}{}",
            style.dim,
            failed,
            if failed == 1 { "" } else { "s" },
            style.reset,
        );
    }
    let mut chat_key: Option<String> = None;

    // One long-running loop drives every turn; each user input flips the
    // ticket out of the gate's pause and the next iteration redraws the
    // prompt once the assistant has spoken.
    tickets.start();

    loop {
        let line = tokio::select! {
            line = read_line(&user_prompt) => line,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\n{}^C{}", style.dim, style.reset);
                std::process::exit(130);
            }
        };
        let Some(line) = line else {
            std::process::exit(0)
        };
        if line.is_empty() {
            continue;
        }
        if line == "/new" {
            eprintln!("{}usage: /new <message>{}", style.dim, style.reset);
            continue;
        }
        if let Some(first) = line.strip_prefix("/new ") {
            let k = tickets.task(first.trim());
            eprintln!("{}new chat {k}{}", style.dim, style.reset);
            chat_key = Some(k);
            continue;
        }
        if line == "/list" {
            let all = tickets.tickets();
            if all.is_empty() {
                eprintln!("{}(no tickets){}", style.dim, style.reset);
            } else {
                for t in all {
                    let preview = match &t.task {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let mut preview: String = preview.chars().take(60).collect();
                    if preview.len() < t.task.to_string().len() {
                        preview.push('…');
                    }
                    let active = chat_key.as_deref() == Some(t.key.as_str());
                    let mark = if active { "▸ " } else { "  " };
                    eprintln!(
                        "{}{mark}{} [{}] · {} replies · {}{}",
                        style.dim,
                        t.key,
                        t.status,
                        t.replies.len(),
                        preview,
                        style.reset,
                    );
                }
            }
            continue;
        }
        if line == "/stats" {
            let s = tickets.stats();
            eprintln!(
                "{}{} turns · {} requests · {} tools · {} in / {} out · {} created / {} done / {} failed{}",
                style.dim,
                s.turns(),
                s.requests(),
                s.tool_calls(),
                s.input_tokens(),
                s.output_tokens(),
                s.tickets_created(),
                s.tickets_finished(),
                s.tickets_failed(),
                style.reset,
            );
            continue;
        }
        if line == "/clear" {
            knowledge.clear().ok();
            eprintln!("{}knowledge cleared{}", style.dim, style.reset);
            continue;
        }
        if line == "/scrub" {
            eprintln!("{}usage: /scrub <word>{}", style.dim, style.reset);
            continue;
        }
        if let Some(word) = line.strip_prefix("/scrub ") {
            let word = word.trim().to_string();
            match chat_key.as_deref() {
                Some(_) if word.is_empty() => {
                    eprintln!("{}usage: /scrub <word>{}", style.dim, style.reset);
                }
                Some(k) => {
                    tickets.edit_replies(k, move |messages| redact(messages, &word));
                    eprintln!("{}scrubbed{}", style.dim, style.reset);
                }
                None => eprintln!("{}no active chat{}", style.dim, style.reset),
            }
            continue;
        }
        let payload = if line == "/bible" || line.starts_with("/bible ") {
            let argument = line.strip_prefix("/bible").unwrap().trim();
            let repetitions = if argument.is_empty() {
                BIBLE_DEFAULT_REPETITIONS
            } else {
                match argument.parse::<usize>() {
                    Ok(n) if n > 0 => n,
                    _ => {
                        eprintln!(
                            "{}usage: /bible [N]   (positive integer; default {}){}",
                            style.dim, BIBLE_DEFAULT_REPETITIONS, style.reset,
                        );
                        continue;
                    }
                }
            };
            let mut bible_payload = String::with_capacity(BIBLE_PASSAGE.len() * repetitions + 64);
            bible_payload
                .push_str("Read the following passage and reply with a single short sentence.\n\n");
            for _ in 0..repetitions {
                bible_payload.push_str(BIBLE_PASSAGE);
            }
            eprintln!(
                "{}injecting {} repetitions · {} KiB · ~{} input tokens{}",
                style.dim,
                repetitions,
                bible_payload.len() / 1024,
                bible_payload.len() / 4,
                style.reset,
            );
            bible_payload
        } else {
            line
        };

        announce_assistant(&style);
        // "agent › " left stdout mid-line; mark so the first event
        // breaks out before its own content.
        midstream.store(true, Ordering::Relaxed);
        let key = match chat_key.as_deref() {
            Some(k)
                if tickets
                    .tickets()
                    .iter()
                    .any(|t| t.key == k && t.is_in_progress()) =>
            {
                tickets.reply(k, payload);
                k.to_string()
            }
            _ => tickets.task(payload),
        };
        chat_key = Some(key.clone());

        let cancelled = tokio::select! {
            _ = wait_for_assistant_pause(&tickets, &key) => false,
            _ = tokio::signal::ctrl_c() => {
                tickets.cancel();
                if midstream.swap(false, Ordering::Relaxed) {
                    eprintln!();
                }
                eprintln!("{}cancelling…{}", style.dim, style.reset);
                let drain_fut = tickets.finish();
                tokio::pin!(drain_fut);
                tokio::select! {
                    _ = &mut drain_fut => {}
                    _ = tokio::signal::ctrl_c() => std::process::exit(130),
                }
                tickets.start();
                true
            }
        };

        let stats = tickets.stats();
        let outcome = {
            let chat = chat_key
                .as_deref()
                .and_then(|k| tickets.tickets().into_iter().find(|t| t.key == k));
            match chat {
                Some(t) if t.is_finished() => {
                    chat_key = None;
                    "completed"
                }
                Some(t) if t.is_failed() => {
                    chat_key = None;
                    "failed"
                }
                _ if cancelled => "cancelled",
                _ => "incomplete",
            }
        };

        let turns = stats.turns().saturating_sub(prev_turns);
        let requests = stats.requests().saturating_sub(prev_requests);
        let tool_calls = stats.tool_calls().saturating_sub(prev_tool_calls);
        let input = stats.input_tokens().saturating_sub(prev_input);
        let output = stats.output_tokens().saturating_sub(prev_output);
        prev_turns = stats.turns();
        prev_requests = stats.requests();
        prev_tool_calls = stats.tool_calls();
        prev_input = stats.input_tokens();
        prev_output = stats.output_tokens();

        if midstream.swap(false, Ordering::Relaxed) {
            eprintln!();
        }
        eprintln!(
            "{}{outcome} · {turns} turns · {requests} requests · {tool_calls} tools · {input} in / {output} out{}",
            style.dim, style.reset,
        );
    }
}

fn announce_assistant(style: &Style) {
    print!("\n{}agent ›{} ", style.agent, style.reset);
    let _ = io::stdout().flush();
}

fn print_event(
    event: &Event,
    style: &Style,
    window: Option<u64>,
    midstream: &AtomicBool,
    last_input: &AtomicU64,
) {
    // Emit a single leading newline only when streamed model text just
    // landed on stdout without a trailing newline; subsequent events
    // print directly on their own line.
    let break_stream = || {
        if midstream.swap(false, Ordering::Relaxed) {
            eprintln!();
        }
    };
    match &event.kind {
        EventKind::TextChunkReceived { content } => {
            print!("{content}");
            let _ = io::stdout().flush();
            midstream.store(true, Ordering::Relaxed);
        }
        EventKind::ToolCallStarted {
            tool_name, input, ..
        } => {
            break_stream();
            let arg = input["pattern"]
                .as_str()
                .or_else(|| input["path"].as_str())
                .or_else(|| input["query"].as_str())
                .unwrap_or("");
            if arg.is_empty() {
                eprintln!("{}· {tool_name}{}", style.dim, style.reset);
            } else {
                eprintln!("{}· {tool_name}({arg}){}", style.dim, style.reset);
            }
        }
        EventKind::ToolCallFailed {
            tool_name, message, ..
        } => {
            break_stream();
            eprintln!("{}✗ {tool_name}: {message}{}", style.red, style.reset);
        }
        EventKind::RequestFinished { usage, .. } => {
            last_input.store(usage.input_tokens, Ordering::Relaxed);
            if let Some(window) = window {
                break_stream();
                let used = usage.input_tokens;
                let remaining = window.saturating_sub(used);
                let threshold = window.saturating_mul(7) / 10;
                let (marker, color) = if used >= threshold {
                    ("⚠", style.red)
                } else {
                    ("·", style.dim)
                };
                eprintln!(
                    "{color}{marker} {used} / {window} tokens used ({remaining} left, warn at {threshold}){reset}",
                    reset = style.reset,
                );
            }
        }
        EventKind::CompactionStarted { reason, total } => {
            break_stream();
            eprintln!(
                "{}… compacting context ({reason:?}): {total} chunks{}{}",
                style.dim,
                window_usage_suffix(window, last_input),
                style.reset,
            );
        }
        EventKind::CompactionProgress {
            reason: _,
            completed,
            total,
        } => {
            break_stream();
            eprintln!("{}  ▸ {completed}/{total}{}", style.dim, style.reset,);
        }
        EventKind::CompactionFinished { reason } => {
            break_stream();
            eprintln!(
                "{}✓ context compacted ({reason:?}){}{}",
                style.dim,
                window_usage_suffix(window, last_input),
                style.reset,
            );
        }
        EventKind::CompactionFailed { reason, message } => {
            break_stream();
            eprintln!(
                "{}✗ compaction failed ({reason:?}){}{}",
                style.red,
                window_usage_suffix(window, last_input),
                style.reset,
            );
            print_indented_detail(message, style);
        }
        EventKind::RequestFailed {
            reason, message, ..
        } => {
            break_stream();
            eprintln!("{}✗ request failed ({reason:?}){}", style.red, style.reset);
            print_indented_detail(message, style);
        }
        EventKind::RequestRetried {
            attempt,
            max_attempts,
            reason,
            message,
            ..
        } => {
            break_stream();
            eprintln!(
                "{}↻ retry {attempt}/{max_attempts} ({reason:?}){}",
                style.dim, style.reset,
            );
            print_indented_detail(message, style);
        }
        EventKind::SchemaRetried { .. } => {}
        EventKind::PolicyViolated { policy, limit } => {
            break_stream();
            eprintln!(
                "{}✗ policy {policy:?} (limit {limit}){}",
                style.red, style.reset,
            );
        }
        _ => {}
    }
}

fn print_indented_detail(message: &str, style: &Style) {
    for line in message.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            eprintln!("{}    {trimmed}{}", style.dim, style.reset);
        }
    }
}

fn window_usage_suffix(window: Option<u64>, last_input: &AtomicU64) -> String {
    let used = last_input.load(Ordering::Relaxed);
    match window {
        Some(window) => {
            let remaining = window.saturating_sub(used);
            format!(" · {used} / {window} tokens used, {remaining} left")
        }
        None => String::new(),
    }
}

/// Replace every occurrence of `word` with `[redacted]` across the transcript.
fn redact(messages: &mut [Reply], word: &str) {
    for reply in messages.iter_mut() {
        for block in &mut reply.content {
            if let ReplyContent::Text { text } = block {
                *text = text.replace(word, "[redacted]");
            }
        }
    }
}

/// Block until the chat ticket is either terminal (`finished`/`failed`)
/// or paused for input. A mid-turn assistant reply carrying a tool call
/// doesn't count: tool execution is still pending and the prompt would
/// race the user against the loop.
async fn wait_for_assistant_pause(tickets: &TicketSystem, key: &str) {
    tickets
        .wait_for_ticket(|t| t.key == key && (t.is_resolved() || is_paused_for_input(t)))
        .await;
}

/// The loop has paused for the next input when the last reply is an
/// assistant turn that called no tool. Reasoning models add `Thinking`
/// blocks alongside the text, so the pause turns on the absence of a
/// tool call, not on the reply being text-only.
fn is_paused_for_input(ticket: &Ticket) -> bool {
    ticket.replies.last().is_some_and(|r| {
        r.author == Author::Assistant
            && !r
                .content
                .iter()
                .any(|c| matches!(c, ReplyContent::ToolUse { .. }))
    })
}

async fn read_line(prompt: &str) -> Option<String> {
    print!("{prompt}");
    io::stdout().flush().ok()?;
    tokio::task::spawn_blocking(|| io::stdin().lines().next()?.ok().map(|s| s.trim().into()))
        .await
        .ok()
        .flatten()
}

#[derive(Clone)]
struct Style {
    dim: &'static str,
    user: &'static str,
    agent: &'static str,
    red: &'static str,
    reset: &'static str,
}

impl Style {
    fn detect() -> Self {
        if io::stdout().is_terminal() && io::stderr().is_terminal() {
            Self {
                dim: "\x1b[2m",
                user: "\x1b[1;33m",
                agent: "\x1b[1;36m",
                red: "\x1b[31m",
                reset: "\x1b[0m",
            }
        } else {
            Self {
                dim: "",
                user: "",
                agent: "",
                red: "",
                reset: "",
            }
        }
    }
}

/// Transition every non-terminal ticket carrying `label` to Failed.
/// Catches both the active InProgress chat from the prior session and
/// any orphan Todo left by an interrupted `/new <message>`.
fn fail_stale_chats(tickets: &TicketSystem, label: &str) -> usize {
    let stale: Vec<String> = tickets
        .find_tickets(|t| t.is_pending() && t.has_label(label))
        .iter()
        .map(|t| t.key.clone())
        .collect();
    for key in &stale {
        let _ = tickets.set_failed(key);
    }
    stale.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentwerk::agents::tickets::{Status, Ticket};

    #[test]
    fn fail_stale_chats_marks_every_matching_pending_ticket_as_failed() {
        let tickets = TicketSystem::new();
        let mut keys = Vec::new();
        for body in ["one", "two", "three"] {
            let k = tickets.ticket(Ticket::new(body).label("orchestrator"));
            keys.push(k);
        }
        let other = tickets.ticket(Ticket::new("scanner").label("analyst"));

        let n = fail_stale_chats(&tickets, "orchestrator");

        assert_eq!(n, 3);
        for k in &keys {
            assert_eq!(tickets.get_ticket(k).unwrap().status, Status::Failed);
        }
        assert_eq!(tickets.get_ticket(&other).unwrap().status, Status::Todo);
    }

    #[test]
    fn fail_stale_chats_returns_zero_when_no_matching_tickets_exist() {
        let tickets = TicketSystem::new();
        let n = fail_stale_chats(&tickets, "orchestrator");
        assert_eq!(n, 0);
    }

    fn assistant(content: Vec<ReplyContent>) -> Ticket {
        let mut ticket = Ticket::new("chat");
        ticket.replies.push(Reply {
            author: Author::Assistant,
            content,
            created_at: 0,
        });
        ticket
    }

    #[test]
    fn paused_for_input_when_a_reasoning_reply_carries_thinking_then_text() {
        // Regression: reasoning models add a Thinking block, which the
        // old text-only check rejected, hanging the REPL turn forever.
        let ticket = assistant(vec![
            ReplyContent::Thinking {
                thinking: "hmm".into(),
                signature: "s".into(),
            },
            ReplyContent::Text { text: "Hi.".into() },
        ]);
        assert!(is_paused_for_input(&ticket));
    }

    #[test]
    fn not_paused_while_the_reply_still_carries_a_tool_call() {
        let ticket = assistant(vec![ReplyContent::ToolUse {
            id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({}),
        }]);
        assert!(!is_paused_for_input(&ticket));
    }

    #[test]
    fn not_paused_when_the_last_reply_is_the_user() {
        let mut ticket = Ticket::new("chat");
        ticket.replies.push(Reply {
            author: Author::User,
            content: vec![ReplyContent::Text { text: "hey".into() }],
            created_at: 0,
        });
        assert!(!is_paused_for_input(&ticket));
    }

    fn user_text(text: &str) -> Reply {
        Reply {
            author: Author::User,
            content: vec![ReplyContent::Text { text: text.into() }],
            created_at: 0,
        }
    }

    #[test]
    fn redact_replaces_the_word_in_every_reply() {
        let mut messages = vec![
            user_text("my token is hunter2"),
            Reply {
                author: Author::Assistant,
                content: vec![ReplyContent::Text {
                    text: "noted, hunter2".into(),
                }],
                created_at: 0,
            },
        ];

        redact(&mut messages, "hunter2");

        for reply in &messages {
            for block in &reply.content {
                if let ReplyContent::Text { text } = block {
                    assert!(!text.contains("hunter2"), "leaked: {text}");
                    assert!(text.contains("[redacted]"));
                }
            }
        }
    }

    #[test]
    fn redact_leaves_text_without_the_word_untouched() {
        let mut messages = vec![user_text("just chatting")];
        redact(&mut messages, "hunter2");
        assert!(
            matches!(&messages[0].content[0], ReplyContent::Text { text: t } if t == "just chatting")
        );
    }
}

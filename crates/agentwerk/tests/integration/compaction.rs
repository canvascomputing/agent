//! Core check on summarizing: the configured 4 096-token window makes
//! `compaction_threshold` saturate to 0, so proactive compaction fires
//! between turns. The loop calls compact and the summariser must return
//! non-empty text. The ticket does not need to complete: verifying the
//! summariser is the sole purpose of this test.

use std::sync::{Arc, Mutex};

use super::common;

use agentwerk::agents::tickets::Author;
use agentwerk::event::EventKind;
use agentwerk::providers::Model;
use agentwerk::{Agent, Event, Ticket, TicketSystem};

// Compaction threshold = max(0, 4 096 − 33 000) = 0, so any non-empty
// transcript trips the proactive guard between turns. The task itself
// is ~2 400 tokens; the first model response appends to `usage_history`
// and the next iteration's proactive check fires.
const LOCAL_CTX: u64 = 4_096;

// A realistic debugging scenario: ~1 500 tokens, with enough structure
// (logs, code, hypotheses) to produce a meaningful summary rather than
// just echoing the input.
const TASK: &str = "\
You are helping investigate a production incident. Here is the full context
collected so far. Summarise the key findings and propose a next debugging step.

## Incident: payment-service OOM crash (2024-11-14 03:17 UTC)

### Alert

  CRITICAL  payment-service pod restarted (OOMKilled)
  node: worker-3  namespace: prod
  container memory limit: 512 Mi  actual RSS at crash: 511 Mi

### Recent logs (last 200 lines before crash)

  03:16:52 INFO  [checkout] order=8821 starting charge attempt 1
  03:16:52 DEBUG [stripe]   POST /v1/charges body_bytes=412
  03:16:52 INFO  [checkout] order=8821 charge succeeded charge_id=ch_3OA...
  03:16:52 INFO  [checkout] order=8821 persisting receipt
  03:16:52 DEBUG [db]       BEGIN
  03:16:52 DEBUG [db]       INSERT INTO receipts ...  (ok)
  03:16:52 DEBUG [db]       COMMIT
  03:16:53 INFO  [checkout] order=8822 starting charge attempt 1
  ... (147 similar lines, order ids 8822–8968) ...
  03:17:44 WARN  [runtime]  101 tasks are currently parked waiting for a Mutex
  03:17:44 WARN  [runtime]  task queue depth: 4 819
  03:17:44 ERROR [checkout] order=8969 charge attempt 1 timed out after 30s
  03:17:44 ERROR [checkout] order=8969 charge attempt 2 timed out after 30s
  03:17:44 ERROR [checkout] order=8969 charge attempt 3: stripe error: rate_limit
  03:17:45 ERROR [checkout] FATAL: thread 'tokio-runtime-worker' panicked:
    'called `Result::unwrap()` on an `Err` value: SendError { .. }'
    stack backtrace:
       0: std::panicking::begin_panic
       1: tokio::sync::mpsc::Sender<T>::send (channel.rs:412)
       2: payment_service::audit::emit_event (audit.rs:88)
       3: payment_service::checkout::process_order (checkout.rs:231)
       4: tokio::runtime::task::harness::poll_future (harness.rs:168)

### Relevant source: checkout.rs (lines 218–245)

```rust
pub async fn process_order(order: Order, ctx: Arc<AppContext>) -> Result<Receipt> {
    let charge = ctx.stripe.charge(&order).await?;

    // Write receipt to DB.
    let receipt = ctx.db.insert_receipt(&charge).await?;

    // Notify audit log, fire and forget.
    let audit_tx = ctx.audit_tx.clone();
    tokio::spawn(async move {
        // BUG CANDIDATE: audit_tx is an unbounded channel created once at
        // startup; receiver lives in audit.rs:run_loop().
        audit_tx.send(AuditEvent::OrderCharged(receipt.clone())).unwrap(); // line 231
    });

    Ok(receipt)
}
```

### Relevant source: audit.rs (lines 78–95)

```rust
pub async fn run_loop(mut rx: mpsc::Receiver<AuditEvent>) {
    while let Some(event) = rx.recv().await {
        // Writes to Postgres. Under load this blocks the receiver.
        if let Err(e) = db_write_audit(&event).await {
            error!('audit write failed: {e}');
            // ERROR: receiver just fell behind, channel keeps filling.
        }
    }
}
```

### Hypotheses investigated so far

1. **Stripe rate-limiting (ruled out)**: rate_limit error appeared only on
   order 8969; the 147 preceding orders succeeded. Not the root cause.

2. **DB connection pool exhaustion (inconclusive)**: pool_size=20, observed
   max 18 connections in-use. Unlikely bottleneck but not eliminated.

3. **Audit channel back-pressure (likely)**: audit.rs:run_loop writes to
   Postgres synchronously inside the recv loop. Under the burst of ~120
   orders/min the DB write latency (avg 180 ms) caused the receiver to fall
   behind. The unbounded mpsc channel buffered ~4 800 events before the
   RSS limit was hit. The `unwrap()` on line 231 then panicked once the
   Tokio runtime began shedding tasks and the channel's internal Arc was
   dropped.

### Question

Given the above, what is the single most important code change to make the
audit pipeline robust under back-pressure, and how should it be tested?

Answer in plain prose, no tools.";

#[tokio::test]
async fn summariser_produces_text_when_compaction_fires_against_live_llm() {
    let (provider, model) = common::build_provider();
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let log = events.clone();

    eprintln!("\n=== BEFORE COMPACTION ===\n{TASK}\n");

    let tickets = TicketSystem::new();
    // Two iterations: turn 1 lets the model respond once (appending one
    // entry to `usage_history`); turn 2's proactive guard then trips
    // because `compaction_threshold(LOCAL_CTX)` saturates to 0.
    tickets.max_turns(2);
    tickets.on_event(move |e| log.lock().unwrap().push(e.clone()));
    tickets.agent(
        Agent::new()
            .provider(provider)
            .model(Model::from_name(&model).context_window(LOCAL_CTX))
            .role("{context}\n\nAnswer the question in plain text. Do not call any tools.")
            .build(),
    );
    tickets.ticket(Ticket::new(TASK));
    assert!(
        tickets.last_result().is_none(),
        "no result before run starts"
    );

    tickets.finish().await;

    let all_events = events.lock().unwrap();

    eprintln!("\n=== EVENTS ===");
    for e in all_events.iter() {
        eprintln!("  {:?}", e.kind);
    }

    let compacted = all_events
        .iter()
        .any(|e| matches!(e.kind, EventKind::CompactionStarted { .. }));
    let compaction_succeeded = all_events
        .iter()
        .any(|e| matches!(e.kind, EventKind::CompactionFinished { .. }));

    eprintln!("\n=== AFTER COMPACTION ===");
    for ticket in tickets.tickets() {
        eprintln!("{}", serde_json::to_string_pretty(&ticket).unwrap());
    }

    common::print_result(&tickets, tickets.stats());

    assert!(
        compacted,
        "CompactionStarted must fire before the first request (compaction threshold exceeded)"
    );
    assert!(
        compaction_succeeded,
        "CompactionFinished must fire: summariser must return text"
    );

    // The summary replaces all non-system comments in the ticket. Verify
    // it is substantive: a degenerate "ok" or empty response would pass
    // CompactionFinished but fail here.
    let summary_chars: usize = tickets
        .tickets()
        .iter()
        .flat_map(|t| {
            t.replies
                .iter()
                .filter(|c| c.author == Author::User)
                .flat_map(|c| {
                    serde_json::to_value(c)
                        .ok()
                        .and_then(|v| v["content"].as_array().map(|a| a.to_owned()))
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|block| block["Text"].as_str().map(|s| s.len()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .sum();
    assert!(
        summary_chars >= 200,
        "compaction summary must be at least 200 chars, got {summary_chars}"
    );

    // `reset_usage` runs inside `summarize`, so the per-ticket history
    // can hold at most the entries recorded *after* compaction committed
    // (turn 2's reply). Without the reset, the pre-compaction entry
    // would still be there too, length would be 2.
    for ticket in tickets.tickets() {
        let history = tickets.stats().usage_history(&ticket.key);
        assert!(
            history.len() <= 1,
            "{}: usage_history should be cleared on compaction, found {} entries",
            ticket.key,
            history.len(),
        );
    }
}

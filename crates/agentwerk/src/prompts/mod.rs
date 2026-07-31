//! Assembles what an agent is told: the role, the directives, and the facts
//! `{context}` expands to.

mod builder;
mod section;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub(crate) use builder::PromptBuilder;

use crate::agents::policy::Policies;
use crate::agents::stats::Stats;

const CONTEXT_TEMPLATE: &str = include_str!("context.md");

const RETRY_TEMPLATE: &str = include_str!("retry.directive.md");

const COMPACTION_TEMPLATE: &str = include_str!("compaction.directive.md");

/// Render the corrective user message the loop pushes when the model's
/// previous reply could not finalise the current piece of work. Used
/// for two cases: a finish tool returned a schema-validation error,
/// or the model ended its turn without calling any finish tool.
/// Callers compose a self-contained `detail` describing what was
/// wrong; the template wraps it with the consistent framing.
pub(crate) fn retry_directive(detail: &str) -> String {
    RETRY_TEMPLATE.replace("{detail}", detail)
}

/// System prompt used by the agent loop when it collapses an
/// over-budget conversation into a single summary message. Has no
/// placeholders; the conversation itself is sent as the user messages.
pub(crate) fn compaction_directive() -> &'static str {
    COMPACTION_TEMPLATE
}

/// Render the model-facing block that tells the agent how to return a result
/// matching `schema`, with the document as pretty JSON. Leads with a blank line
/// so callers append it directly after preceding text. Shared by the ticket seed
/// message and the schema-retry directive so the wording stays in step. An
/// object schema is passed as the finish tool's top-level arguments; any other
/// shape rides in `result`, the only place a scalar or array fits.
pub(crate) fn schema_directive(schema: &Value) -> String {
    let pretty = serde_json::to_string_pretty(schema).unwrap_or_default();
    if is_object_schema(schema) {
        format!("\n\nCall `finish` with these fields as its top-level arguments, not wrapped in `result`, matching this schema:\n{pretty}")
    } else {
        format!(
            "\n\nRecord your `result` via `finish` as a JSON value matching this schema:\n{pretty}"
        )
    }
}

/// Compose the detail string for a schema-validation retry. Plugged
/// into `retry_directive` when a finish tool's output does not match
/// the ticket's schema. The validator names what was wrong but not the
/// target shape, so the schema is rendered via [`schema_directive`] and
/// appended when known: without it a model that guessed the shape has
/// nothing new to correct against. Object-schema tickets are always finished
/// via a finish tool that inlines, so the argument wording is correct there.
pub(crate) fn schema_retry_detail(validator_message: &str, schema: Option<&Value>) -> String {
    let shape = schema.map(schema_directive).unwrap_or_default();
    let lead = if schema.is_some_and(is_object_schema) {
        "The arguments you passed did not match the ticket's schema. Call `finish` \
         again with the fields as its top-level arguments that match it."
    } else {
        "The `result` you passed did not match the ticket's schema. Call `finish` \
         again with `result` set to a JSON value that matches it."
    };
    format!("{lead} Validator said: {validator_message}{shape}")
}

fn is_object_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
}

/// Build the body `{context}` expands to: the ticket key, working
/// directory, platform, OS version, and date, plus a `… remaining`
/// bullet for each `Policies` budget that is `Some(_)`. Budgets left as
/// `None` (unlimited) stay invisible. Pass empty `Policies::default()` and
/// `Stats::new()` when you only want the static facts.
pub(crate) fn context_body(
    dir: &Path,
    policies: &Policies,
    stats: &Stats,
    ticket_key: &str,
) -> String {
    let dir_str = dir.display().to_string();
    let platform = std::env::consts::OS;
    let os_version = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let date = format_current_date();
    let mut body = CONTEXT_TEMPLATE
        .trim_matches('\n')
        .replace("{ticket}", ticket_key)
        .replace("{dir}", &dir_str)
        .replace("{platform}", platform)
        .replace("{os_version}", &os_version)
        .replace("{date}", &date);
    if let Some(extra) = runtime_budgets(policies, stats) {
        body.push('\n');
        body.push_str(&extra);
    }
    body
}

/// Closes the budget bullets. A bare number is telemetry; the
/// consequence is what makes the model pace against it.
const BUDGET_CONSEQUENCE: &str =
    "Execution stops when any budget reaches zero, mid-ticket. Finish before then.";

/// Bullets for each configured budget plus [`BUDGET_CONSEQUENCE`], joined
/// by `\n`. `None` when no budget is set, so the caller can skip the join.
fn runtime_budgets(policies: &Policies, stats: &Stats) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(limit) = policies.max_turns {
        let remaining = u64::from(limit).saturating_sub(stats.turns());
        lines.push(format!("- Turns remaining: {remaining}"));
    }
    if let Some(limit) = policies.max_input_tokens {
        let remaining = limit.saturating_sub(stats.input_tokens());
        lines.push(format!("- Input tokens remaining: {remaining}"));
    }
    if let Some(limit) = policies.max_output_tokens {
        let remaining = limit.saturating_sub(stats.output_tokens());
        lines.push(format!("- Output tokens remaining: {remaining}"));
    }
    if let Some(limit) = policies.max_time {
        if let Some(elapsed) = stats.run_duration() {
            let remaining = limit.saturating_sub(elapsed);
            lines.push(format!("- Time remaining: {}s", remaining.as_secs()));
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.push(format!("\n{BUDGET_CONSEQUENCE}"));
    Some(lines.join("\n"))
}

/// Today's date as `YYYY-MM-DD`, via the civil-from-days algorithm.
fn format_current_date() -> String {
    let epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = epoch_secs / 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;
    use crate::providers::TokenUsage;
    use std::path::PathBuf;
    use std::time::Duration;

    fn turn() -> EventKind {
        EventKind::TurnStarted
    }

    fn request(input_tokens: u64, output_tokens: u64) -> EventKind {
        EventKind::RequestFinished {
            model: "m".into(),
            usage: TokenUsage {
                input_tokens,
                output_tokens,
            },
        }
    }

    #[test]
    fn schema_directive_for_an_object_asks_for_top_level_arguments() {
        let directive = schema_directive(&serde_json::json!({
            "type": "object",
            "properties": {"summary": {"type": "string"}},
            "required": ["summary"],
        }));
        assert!(directive.contains("top-level arguments"));
        assert!(directive.contains("matching this schema"));
        assert!(directive.contains("summary"));
    }

    #[test]
    fn schema_directive_for_a_scalar_keeps_the_result_envelope() {
        let directive = schema_directive(&serde_json::json!({ "type": "string" }));
        assert!(directive.contains("`result`"));
        assert!(directive.contains("finish"));
    }

    #[test]
    fn schema_retry_detail_appends_the_schema_when_known() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"summary": {"type": "string"}},
        });
        let rendered = schema_retry_detail("expected type object, got string", Some(&schema));
        assert!(rendered.contains("expected type object, got string"));
        // The model that guessed wrong now sees the keys it must produce.
        assert!(rendered.contains("matching this schema"));
        assert!(rendered.contains("summary"));
    }

    #[test]
    fn schema_retry_detail_adds_no_shape_without_a_schema() {
        let rendered = schema_retry_detail("expected type object, got string", None);
        assert!(rendered.contains("expected type object, got string"));
        assert!(!rendered.contains("matching this schema"));
    }

    #[test]
    fn retry_directive_substitutes_detail_placeholder() {
        let rendered = retry_directive("expected integer at /partial_sum");
        assert!(rendered.contains("expected integer at /partial_sum"));
        assert!(!rendered.contains("{detail}"));
        assert!(rendered.contains("not accepted"));
    }

    #[test]
    fn context_body_renders_bare_bullets_with_substituted_values() {
        let rendered = context_body(
            &PathBuf::from("/tmp/check"),
            &Policies::default(),
            &Stats::new(),
            "TICKET-7",
        );
        let lines: Vec<&str> = rendered.lines().collect();
        assert!(lines[0].starts_with("You work within a ticket queue."));
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "- Ticket: TICKET-7");
        assert!(lines[3].starts_with("- Date: "));
        assert_eq!(lines[4], "- Working directory: /tmp/check");
        assert!(lines[5].starts_with("- Platform: "));
        assert!(!rendered.contains('{'), "no unsubstituted placeholders");
    }

    #[test]
    fn context_body_carries_no_heading_of_its_own() {
        let rendered = context_body(
            &PathBuf::from("/tmp/check"),
            &Policies::default(),
            &Stats::new(),
            "TICKET-7",
        );
        // The role prompt places the block and owns any heading above it.
        assert!(!rendered.contains("## "));
    }

    #[test]
    fn context_body_lists_each_set_turn_and_token_budget() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_turns: Some(10),
            max_input_tokens: Some(100_000),
            max_output_tokens: Some(20_000),
            ..Policies::default()
        };
        let stats = Stats::new();
        stats.record_event(&turn(), "", &[], "");
        stats.record_event(&turn(), "", &[], "");
        stats.record_event(&request(5_000, 8_000), "", &[], "");

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        // Visualizes the exact appended block. Static prefix (date, directory,
        // platform) is rebuilt with empty policy/stats
        // so the expected literal stays portable across CI hosts.
        let expected = format!(
            "{static_prefix}\n\
             - Turns remaining: 8\n\
             - Input tokens remaining: 95000\n\
             - Output tokens remaining: 12000\n\
             \n{BUDGET_CONSEQUENCE}",
            static_prefix = context_body(&working_dir, &Policies::default(), &Stats::new(), "T-1"),
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn context_body_only_shows_configured_budgets() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_turns: Some(5),
            ..Policies::default()
        };
        let stats = Stats::new();
        stats.record_event(&turn(), "", &[], "");

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        let expected = format!(
            "{static_prefix}\n- Turns remaining: 4\n\n{BUDGET_CONSEQUENCE}",
            static_prefix = context_body(&working_dir, &Policies::default(), &Stats::new(), "T-1"),
        );
        assert_eq!(rendered, expected);
        assert!(!rendered.contains("Input tokens"));
        assert!(!rendered.contains("Output tokens"));
        assert!(!rendered.contains("Time remaining"));
    }

    #[test]
    fn context_body_saturates_remaining_at_zero() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_turns: Some(2),
            ..Policies::default()
        };
        let stats = Stats::new();
        for _ in 0..5 {
            stats.record_event(&turn(), "", &[], "");
        }

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        let expected = format!(
            "{static_prefix}\n- Turns remaining: 0\n\n{BUDGET_CONSEQUENCE}",
            static_prefix = context_body(&working_dir, &Policies::default(), &Stats::new(), "T-1"),
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn context_body_omits_time_when_run_not_started() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_time: Some(Duration::from_secs(300)),
            ..Policies::default()
        };
        let stats = Stats::new();

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        // No `record_started` call: `Stats::run_duration` is `None`,
        // so the time bullet must not appear.
        let baseline = context_body(&working_dir, &Policies::default(), &Stats::new(), "T-1");
        assert_eq!(rendered, baseline);
        assert!(!rendered.contains("Time remaining"));
    }

    #[test]
    fn context_body_includes_time_bullet_once_started() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_time: Some(Duration::from_secs(3600)),
            ..Policies::default()
        };
        let stats = Stats::new();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        stats.record_started(now_ms);

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        // Exact remaining seconds depend on millisecond-level timing
        // (truncating elapsed > 0ms drops one second), so assert the
        // bullet shape and that the value is within the tight live
        // window (3599 or 3600 seconds).
        let baseline = context_body(&working_dir, &Policies::default(), &Stats::new(), "T-1");
        assert!(rendered.starts_with(&baseline));
        let trailing = &rendered[baseline.len()..];
        let expected = |seconds| format!("\n- Time remaining: {seconds}s\n\n{BUDGET_CONSEQUENCE}");
        assert!(
            trailing == expected(3600) || trailing == expected(3599),
            "unexpected runtime block: {trailing:?}",
        );
    }
}

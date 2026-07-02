//! Default context block, and the `Section` / `PromptBuilder` that
//! composes the role prompt and (caller-supplied) directives.

mod builder;
mod section;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub(crate) use builder::PromptBuilder;
pub(crate) use section::Section;

use crate::agents::policy::Policies;
use crate::agents::stats::Stats;

const DEFAULT_CONTEXT_TEMPLATE: &str = include_str!("default.context.md");

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
        format!("\n\nCall `finish_ticket` with these fields as its top-level arguments, not wrapped in `result`, matching this schema:\n{pretty}")
    } else {
        format!("\n\nRecord your `result` via `finish_ticket` as a JSON value matching this schema:\n{pretty}")
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
        "The arguments you passed did not match the ticket's schema. Call `finish_ticket` \
         again with the fields as its top-level arguments that match it."
    } else {
        "The `result` you passed did not match the ticket's schema. Call `finish_ticket` \
         again with `result` set to a JSON value that matches it."
    };
    format!("{lead} Validator said: {validator_message}{shape}")
}

fn is_object_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
}

/// Build the default context body: a `## Context` markdown block with the
/// working directory, platform, OS version, and date, plus a `… remaining`
/// bullet for each `Policies` budget that is `Some(_)`. Budgets left as
/// `None` (unlimited) stay invisible. Pass empty `Policies::default()` and
/// `Stats::new()` when you only want the static facts.
pub(crate) fn default_context(dir: &Path, policies: &Policies, stats: &Stats) -> String {
    let dir_str = dir.display().to_string();
    let platform = std::env::consts::OS;
    let os_version = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let date = format_current_date();
    let mut body = DEFAULT_CONTEXT_TEMPLATE
        .replace("{dir}", &dir_str)
        .replace("{platform}", platform)
        .replace("{os_version}", &os_version)
        .replace("{date}", &date);
    if let Some(extra) = runtime_budgets(policies, stats) {
        body.push('\n');
        body.push_str(&extra);
    }
    Section::context(body).render()
}

/// Bullets for each configured budget, joined by `\n`. `None` when no
/// budget is set, so the caller can skip the join.
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
        None
    } else {
        Some(lines.join("\n"))
    }
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
    use crate::agents::stats::{LoopStats, TicketStats};
    use std::path::PathBuf;
    use std::time::Duration;

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
        assert!(directive.contains("finish_ticket"));
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
    fn default_context_renders_markdown_block_with_substituted_values() {
        let rendered = default_context(
            &PathBuf::from("/tmp/check"),
            &Policies::default(),
            &Stats::new(),
        );
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "## Context");
        assert_eq!(lines[1], "");
        assert!(lines[2].starts_with("You work within a ticket system."));
        assert_eq!(lines[3], "");
        assert!(lines[4].starts_with("- Date: "));
        assert_eq!(lines[5], "- Working directory: /tmp/check");
        assert!(lines[6].starts_with("- Platform: "));
        assert!(!rendered.contains('{'), "no unsubstituted placeholders");
    }

    #[test]
    fn default_context_lists_each_set_turn_and_token_budget() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_turns: Some(10),
            max_input_tokens: Some(100_000),
            max_output_tokens: Some(20_000),
            ..Policies::default()
        };
        let stats = Stats::new();
        stats.record_turn();
        stats.record_turn();
        stats.record_request(5_000, 8_000);

        let rendered = default_context(&working_dir, &policies, &stats);

        // Visualizes the exact appended block. Static prefix (date, directory,
        // platform) is rebuilt with empty policy/stats
        // so the expected literal stays portable across CI hosts.
        let expected = format!(
            "{static_prefix}\n\
             - Turns remaining: 8\n\
             - Input tokens remaining: 95000\n\
             - Output tokens remaining: 12000",
            static_prefix = default_context(&working_dir, &Policies::default(), &Stats::new()),
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn default_context_only_shows_configured_budgets() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_turns: Some(5),
            ..Policies::default()
        };
        let stats = Stats::new();
        stats.record_turn();

        let rendered = default_context(&working_dir, &policies, &stats);

        let expected = format!(
            "{static_prefix}\n- Turns remaining: 4",
            static_prefix = default_context(&working_dir, &Policies::default(), &Stats::new()),
        );
        assert_eq!(rendered, expected);
        assert!(!rendered.contains("Input tokens"));
        assert!(!rendered.contains("Output tokens"));
        assert!(!rendered.contains("Time remaining"));
    }

    #[test]
    fn default_context_saturates_remaining_at_zero() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_turns: Some(2),
            ..Policies::default()
        };
        let stats = Stats::new();
        for _ in 0..5 {
            stats.record_turn();
        }

        let rendered = default_context(&working_dir, &policies, &stats);

        let expected = format!(
            "{static_prefix}\n- Turns remaining: 0",
            static_prefix = default_context(&working_dir, &Policies::default(), &Stats::new()),
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn default_context_omits_time_when_run_not_started() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_time: Some(Duration::from_secs(300)),
            ..Policies::default()
        };
        let stats = Stats::new();

        let rendered = default_context(&working_dir, &policies, &stats);

        // No `record_started` call: `Stats::run_duration` is `None`,
        // so the time bullet must not appear.
        let baseline = default_context(&working_dir, &Policies::default(), &Stats::new());
        assert_eq!(rendered, baseline);
        assert!(!rendered.contains("Time remaining"));
    }

    #[test]
    fn default_context_includes_time_bullet_once_started() {
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

        let rendered = default_context(&working_dir, &policies, &stats);

        // Exact remaining seconds depend on millisecond-level timing
        // (truncating elapsed > 0ms drops one second), so assert the
        // bullet shape and that the value is within the tight live
        // window (3599 or 3600 seconds).
        let baseline = default_context(&working_dir, &Policies::default(), &Stats::new());
        assert!(rendered.starts_with(&baseline));
        let trailing = &rendered[baseline.len()..];
        assert!(
            trailing == "\n- Time remaining: 3600s" || trailing == "\n- Time remaining: 3599s",
            "unexpected runtime block: {trailing:?}",
        );
    }
}

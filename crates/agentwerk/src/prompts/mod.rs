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
use crate::event::EventName;
use crate::schemas::Schema;

const CONTEXT_TEMPLATE: &str = include_str!("context.md");

const RETRY_TEMPLATE: &str = include_str!("retry.directive.md");

const COMPACTION_TEMPLATE: &str = include_str!("compaction.directive.md");

/// Render the corrective message the loop pushes when the model's previous
/// reply did not carry the work forward. `detail` says what was wrong and
/// stands on its own; the template supplies the framing around it.
pub(crate) fn retry_directive(detail: &str) -> String {
    RETRY_TEMPLATE.replace("{detail}", detail)
}

/// The system prompt for collapsing an over-budget conversation into one
/// summary. No placeholders: the messages themselves are what it summarizes.
pub(crate) fn compaction_directive() -> &'static str {
    COMPACTION_TEMPLATE
}

/// Render the block telling the agent how to return a result matching
/// `schema`. Leads with a blank line so callers append it directly after
/// preceding text.
pub(crate) fn schema_directive(schema: &Schema) -> String {
    let pretty = serde_json::to_string_pretty(schema.get_raw_schema()).unwrap_or_default();
    format!("\n\nRecord your `result` via `finish` as a JSON value matching this schema:\n{pretty}")
}

/// Compose the detail for a call whose arguments did not match its tool's
/// schema. The validator names what was wrong but not the target shape, so the
/// schema is appended when known: without it a model that guessed the shape has
/// nothing new to correct against.
pub(crate) fn arguments_retry_detail(
    tool_name: &str,
    violations: &str,
    schema: Option<&Value>,
) -> String {
    let shape = schema
        .map(|schema| {
            let pretty = serde_json::to_string_pretty(schema).unwrap_or_default();
            format!("\n\nThe arguments `{tool_name}` accepts:\n{pretty}")
        })
        .unwrap_or_default();
    format!(
        "`{tool_name}` rejected your arguments. Call it again with arguments \
         that match its schema.\n\n{violations}{shape}"
    )
}

/// Every fact the context knows, as `(placeholder, value)`. An unlimited
/// budget carries an empty value, which drops its bullet in
/// [`render_context`]. Pass `Policies::default()` and `Stats::new()` for the
/// static facts alone.
pub(crate) fn context_values(
    dir: &Path,
    policies: &Policies,
    stats: &Stats,
    ticket_key: &str,
) -> Vec<(&'static str, String)> {
    let os_version = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let turns = policies
        .max_turns
        .map(|limit| u64::from(limit).saturating_sub(stats.event_count(EventName::TurnStarted)));
    let input_tokens = policies
        .max_input_tokens
        .map(|limit| limit.saturating_sub(stats.input_tokens()));
    let output_tokens = policies
        .max_output_tokens
        .map(|limit| limit.saturating_sub(stats.output_tokens()));
    let time = policies.max_time.zip(stats.execution_duration());
    vec![
        ("ticket", ticket_key.to_string()),
        ("date", format_current_date()),
        ("dir", dir.display().to_string()),
        ("platform", std::env::consts::OS.to_string()),
        ("os_version", os_version),
        ("turns_remaining", optional(turns)),
        ("input_tokens_remaining", optional(input_tokens)),
        ("output_tokens_remaining", optional(output_tokens)),
        (
            "time_remaining",
            optional(
                time.map(|(limit, elapsed)| {
                    format!("{}s", limit.saturating_sub(elapsed).as_secs())
                }),
            ),
        ),
    ]
}

/// An unset budget renders as no value at all, never as a bare `0`.
fn optional(value: Option<impl ToString>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// Build the bullet list `{context}` expands to. Every label lives in
/// `context.md`; a line whose placeholders all came back empty is left out, so
/// an unconfigured budget shows no bullet. One empty value next to a filled one
/// keeps the line: a failed `uname` still leaves a platform.
pub(crate) fn render_context(values: &[(&'static str, String)]) -> String {
    let lines: Vec<String> = CONTEXT_TEMPLATE
        .trim_matches('\n')
        .lines()
        .filter_map(|line| {
            let mut rendered = line.to_string();
            let mut carries_value = false;
            for (name, value) in values {
                let placeholder = format!("{{{name}}}");
                if rendered.contains(&placeholder) {
                    carries_value |= !value.is_empty();
                    rendered = rendered.replace(&placeholder, value);
                }
            }
            carries_value.then(|| rendered.trim_end().to_string())
        })
        .collect();
    lines.join("\n")
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
    fn every_schema_shape_asks_for_the_result_argument() {
        let shapes = [
            serde_json::json!({
                "type": "object",
                "properties": {"summary": {"type": "string"}},
                "required": ["summary"],
            }),
            // A ticket field named like a control key needs no special prose:
            // it sits inside `result`, not next to `handover`.
            serde_json::json!({
                "type": "object",
                "properties": { "handover": { "type": "string" } },
            }),
            serde_json::json!({ "type": "string" }),
        ];

        for shape in shapes {
            let directive = schema_directive(&Schema::new(shape).expect("valid schema"));
            assert!(directive.contains("`result`"), "{directive}");
            assert!(directive.contains("matching this schema"), "{directive}");
        }
    }

    #[test]
    fn schema_directive_renders_the_schema_itself() {
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"summary": {"type": "string"}},
        }))
        .expect("valid schema");

        assert!(schema_directive(&schema).contains("summary"));
    }

    #[test]
    fn arguments_retry_detail_names_the_tool_and_its_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"offset": {"type": "integer"}},
        });
        let rendered =
            arguments_retry_detail("read_file", "/offset: expected type integer", Some(&schema));
        assert!(rendered.contains("read_file"));
        assert!(rendered.contains("/offset: expected type integer"));
        assert!(rendered.contains("offset"));
        // Pointing at `finish` is the mistake this wording exists to avoid.
        assert!(!rendered.contains("finish"), "{rendered}");
    }

    #[test]
    fn arguments_retry_detail_adds_no_shape_without_a_schema() {
        let rendered = arguments_retry_detail("read_file", "/offset: expected type integer", None);
        assert!(rendered.contains("read_file"));
        assert!(!rendered.contains("accepts:"));
    }

    #[test]
    fn retry_directive_substitutes_detail_placeholder() {
        let rendered = retry_directive("expected integer at /partial_sum");
        assert!(rendered.contains("expected integer at /partial_sum"));
        assert!(!rendered.contains("{detail}"));
        assert!(rendered.contains("not accepted"));
    }

    fn context_body(
        dir: &std::path::Path,
        policies: &Policies,
        stats: &Stats,
        ticket_key: &str,
    ) -> String {
        render_context(&context_values(dir, policies, stats, ticket_key))
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
        assert_eq!(lines[0], "- Ticket: TICKET-7");
        assert!(lines[1].starts_with("- Date: "));
        assert_eq!(lines[2], "- Working directory: /tmp/check");
        assert!(lines[3].starts_with("- Platform: "));
        assert!(!rendered.contains('{'), "no unsubstituted placeholders");
    }

    #[test]
    fn context_body_carries_no_prose_around_the_bullets() {
        let rendered = context_body(
            &PathBuf::from("/tmp/check"),
            &Policies::default(),
            &Stats::new(),
            "TICKET-7",
        );
        // The role prompt owns any framing and heading around the facts.
        assert!(rendered.lines().all(|line| line.starts_with("- ")));
        assert!(!rendered.contains("## "));
    }

    #[test]
    fn context_values_leave_an_unset_budget_empty() {
        let values = context_values(
            &PathBuf::from("/tmp/check"),
            &Policies::default(),
            &Stats::new(),
            "TICKET-7",
        );
        let value = |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value("ticket"), Some("TICKET-7"));
        assert_eq!(value("turns_remaining"), Some(""));
        assert_eq!(value("time_remaining"), Some(""));
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
        let stats = Stats::of([turn(), turn(), request(5_000, 8_000)]);

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        // The static prefix is rebuilt rather than written out, so the expected
        // literal stays portable across hosts.
        let expected = format!(
            "{static_prefix}\n\
             - Turns remaining: 8\n\
             - Input tokens remaining: 95000\n\
             - Output tokens remaining: 12000",
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
        let stats = Stats::of([turn()]);

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        let expected = format!(
            "{static_prefix}\n- Turns remaining: 4",
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
        let stats = Stats::of(std::iter::repeat_n(turn(), 5));

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        let expected = format!(
            "{static_prefix}\n- Turns remaining: 0",
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

        // No ticket ever started, so `Stats::execution_duration` is `None` and
        // the time bullet must not appear.
        let baseline = context_body(&working_dir, &Policies::default(), &Stats::new(), "T-1");
        assert_eq!(rendered, baseline);
        assert!(!rendered.contains("Time remaining"));
    }

    #[test]
    fn context_body_includes_time_bullet_once_started() {
        let working_dir = PathBuf::from("/tmp/check");
        let policies = Policies {
            max_time: Some(Duration::from_secs(3600)),
            ..Policies::default()
        };
        let stats = Stats::of([EventKind::TicketStarted]);

        let rendered = context_body(&working_dir, &policies, &stats, "T-1");

        // Truncating an elapsed duration above 0ms drops one second, so both
        // 3600 and 3599 are correct here.
        let baseline = context_body(&working_dir, &Policies::default(), &Stats::new(), "T-1");
        assert!(rendered.starts_with(&baseline));
        let trailing = &rendered[baseline.len()..];
        let expected = |seconds| format!("\n- Time remaining: {seconds}s");
        assert!(
            trailing == expected(3600) || trailing == expected(3599),
            "unexpected runtime block: {trailing:?}",
        );
    }
}

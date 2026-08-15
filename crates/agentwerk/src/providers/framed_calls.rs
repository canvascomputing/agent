//! Recovers the calls a model wrote as text rather than emitting them through
//! the tool channel. The syntax leaves the reply only once the calls in it are
//! being run; a reply whose calls are all declined keeps the text it wrote,
//! which is then all that is left of them.
//!
//! These models are trained to write a call as
//! `<tool_call><function=NAME><parameter=KEY>VALUE</parameter>…</function></tool_call>`
//! for the endpoint to convert. They do not do it consistently: in one run the
//! same model emits some calls properly and writes others as text, and it also
//! writes a call it did emit. A call left as text costs the turn: the loop
//! answers "no tool call", or the tool answers "missing required parameter" for
//! one delivered empty. What the model wrote is still in the reply, so the call
//! is read from there.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::provider::ProviderToolDefinition;
use super::types::{ContentBlock, ModelResponse, ResponseStatus, StreamEvent, ToolDeclineKind};

const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";
const FUNCTION_OPEN: &str = "<function=";
const FUNCTION_CLOSE: &str = "</function>";
const PARAMETER_OPEN: &str = "<parameter=";
const PARAMETER_CLOSE: &str = "</parameter>";

/// Nothing is invented: a call runs only if the reply ended of the model's own
/// accord. A name no tool answers to still runs, so the registry's error names
/// the tools that exist rather than the call vanishing into the reply text.
pub(crate) fn repair(
    response: &mut ModelResponse,
    tools: &[ProviderToolDefinition],
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let framed = find_framed_calls(response);
    if framed.is_empty() {
        return;
    }
    // Read before the syntax leaves the reply: a batch nothing will run keeps
    // the text the model wrote, which is then all that is left of it.
    if let Some(reason) = decline_reason(&response.status) {
        for call in &framed {
            decline(call, reason, on_event);
        }
        return;
    }
    strip_framed_syntax(response);
    apply_framed_calls(response, tools, &framed, on_event);
}

/// Read every framed call the reply carries, leaving the reply as it is.
fn find_framed_calls(response: &ModelResponse) -> Vec<FramedCall> {
    let mut framed = Vec::new();
    for block in &response.content {
        // Thinking is read but never stripped: the endpoint takes the block
        // back verbatim.
        let text = match block {
            ContentBlock::Text { text } => text,
            ContentBlock::Thinking { thinking, .. } => thinking,
            _ => continue,
        };
        framed.extend(split_framed_calls(text).1);
    }
    framed
}

/// Remove the syntax of every framed call from the visible text, now that the
/// batch is known to be processed.
fn strip_framed_syntax(response: &mut ModelResponse) {
    for block in &mut response.content {
        if let ContentBlock::Text { text } = block {
            *text = split_framed_calls(text).0;
        }
    }
    response
        .content
        .retain(|block| !matches!(block, ContentBlock::Text { text } if text.is_empty()));
}

fn decline(
    call: &FramedCall,
    reason: ToolDeclineKind,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    on_event(StreamEvent::ToolCallDeclined {
        tool_name: call.name.clone(),
        reason,
    });
}

fn decline_reason(status: &ResponseStatus) -> Option<ToolDeclineKind> {
    // Any terminal but the model's own can end a reply just past a complete
    // `</function>`, leaving whole-looking syntax it never committed to.
    match status {
        ResponseStatus::EndTurn | ResponseStatus::ToolUse => None,
        ResponseStatus::OutputTruncated => Some(ToolDeclineKind::OutputTruncated),
        ResponseStatus::StopSequence | ResponseStatus::Refused | ResponseStatus::PauseTurn => {
            Some(ToolDeclineKind::ReplyNotFinished)
        }
    }
}

/// Run each framed call the one way it can run: completing the call the
/// endpoint delivered empty at the same position among its same-named
/// siblings, or adding a call for a name the endpoint delivered nothing under.
/// A name the endpoint did deliver is only ever matched by position, so a call
/// it delivered cannot run twice.
fn apply_framed_calls(
    response: &mut ModelResponse,
    tools: &[ProviderToolDefinition],
    framed: &[FramedCall],
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    // Read before anything is added, so one added call cannot hide a same-named
    // sibling behind it.
    let delivered: HashSet<String> = response
        .content
        .iter()
        .filter_map(tool_call_name)
        .map(str::to_string)
        .collect();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut added = Vec::new();

    for (offset, call) in framed.iter().enumerate() {
        let position = seen.entry(call.name.as_str()).or_default();
        let at = *position;
        *position += 1;
        let typed = typed_arguments(call, schema_for(tools, &call.name));

        let Some(input) = nth_delivered_input(response, &call.name, at) else {
            if delivered.contains(call.name.as_str()) {
                // The endpoint delivered this name already, just not this many
                // times: what the model wrote here has nowhere left to go.
                decline(call, ToolDeclineKind::AlreadyDelivered, on_event);
            } else {
                added.push(ContentBlock::ToolUse {
                    id: format!("repaired_{offset}"),
                    name: call.name.clone(),
                    input: typed,
                });
                repaired(call, on_event);
            }
            continue;
        };

        if input.as_object().is_none_or(|fields| fields.is_empty()) {
            *input = typed;
            repaired(call, on_event);
        } else if *input != typed {
            decline(call, ToolDeclineKind::AlreadyDelivered, on_event);
        }
        // A delivered call whose arguments already match is the call the model
        // wrote twice: nothing to repair, and nothing lost.
    }

    if !added.is_empty() {
        response.content.extend(added);
        // Without the flip the reply reads as an answer and nothing added runs.
        response.status = ResponseStatus::ToolUse;
    }
}

fn tool_call_name(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

/// The arguments of the `at`-th call the endpoint delivered under `name`.
fn nth_delivered_input<'a>(
    response: &'a mut ModelResponse,
    name: &str,
    at: usize,
) -> Option<&'a mut Value> {
    response
        .content
        .iter_mut()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                name: delivered,
                input,
                ..
            } if delivered == name => Some(input),
            _ => None,
        })
        .nth(at)
}

fn repaired(call: &FramedCall, on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>) {
    on_event(StreamEvent::ToolCallRepaired {
        tool_name: call.name.clone(),
    });
}

/// Retype a call's text values against the types its tool advertises, since a
/// parameter declared `boolean` and read with `as_bool()` sees nothing when it
/// arrives as `"true"`.
fn typed_arguments(call: &FramedCall, input_schema: &Value) -> Value {
    let properties = &input_schema["properties"];
    let typed = call
        .arguments
        .iter()
        .map(|(key, value)| (key.clone(), typed_value(value, &properties[key])));
    Value::Object(Map::from_iter(typed))
}

/// A value that does not parse as its declared type stays text, so the tool
/// reports the real problem rather than a guess at what was meant.
fn typed_value(value: &str, schema: &Value) -> Value {
    let fits = |parsed: &Value| match schema["type"].as_str().unwrap_or_default() {
        "boolean" => parsed.is_boolean(),
        "integer" | "number" => parsed.is_number(),
        "array" => parsed.is_array(),
        "object" => parsed.is_object(),
        _ => false,
    };
    serde_json::from_str::<Value>(value)
        .ok()
        .filter(fits)
        .unwrap_or_else(|| Value::String(value.to_string()))
}

/// The arguments schema `name` advertises, or `Value::Null` when the request
/// advertised no such tool. Indexing null yields null, so every value of an
/// unknown call stays the text the model typed.
fn schema_for<'a>(tools: &'a [ProviderToolDefinition], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool.name == name)
        .map_or(&Value::Null, |tool| &tool.input_schema)
}

/// A tool call the model wrote as text instead of emitting it. Every value is
/// the text the model typed; `typed_arguments` retypes them against the tool's
/// own schema.
struct FramedCall {
    name: String,
    arguments: Vec<(String, String)>,
}

/// Split `text` into the prose the model meant to be read and every
/// `<tool_call><function=NAME><parameter=KEY>VALUE</parameter>…</function>
/// </tool_call>` block it carried, in the order they appear. One pass, so what
/// is removed from the reply and what runs can never disagree.
///
/// The `<tool_call>` frame is required. It is the model's own syntax for making
/// a call, which is what separates one from a call it merely wrote about. A
/// block missing any closing tag, or holding something no call could be read
/// from, stays in the prose: the text is then all that is left of it.
fn split_framed_calls(text: &str) -> (String, Vec<FramedCall>) {
    // Every reply on this path is scanned, and almost none carry a call, so the
    // common one costs a search rather than a copy of the whole text.
    if !text.contains(TOOL_CALL_OPEN) {
        return (text.to_string(), Vec::new());
    }
    let mut prose = String::with_capacity(text.len());
    let mut calls = Vec::new();
    let mut rest = text;
    while let Some((before, framed)) = rest.split_once(TOOL_CALL_OPEN) {
        let Some((body, remainder)) = framed.split_once(TOOL_CALL_CLOSE) else {
            break;
        };
        prose.push_str(before);
        match parse_function_block(body) {
            Some(call) => calls.push(call),
            None => {
                prose.push_str(TOOL_CALL_OPEN);
                prose.push_str(body);
                prose.push_str(TOOL_CALL_CLOSE);
            }
        }
        rest = remainder;
    }
    prose.push_str(rest);
    (prose.trim().to_string(), calls)
}

fn parse_function_block(body: &str) -> Option<FramedCall> {
    let (_, after_open) = body.split_once(FUNCTION_OPEN)?;
    let (name, inner) = after_open.split_once('>')?;
    let (parameters, _) = inner.split_once(FUNCTION_CLOSE)?;
    if !is_identifier(name) {
        return None;
    }
    Some(FramedCall {
        name: name.to_string(),
        arguments: parse_parameters(parameters),
    })
}

fn parse_parameters(body: &str) -> Vec<(String, String)> {
    let mut arguments = Vec::new();
    let mut rest = body;
    while let Some((_, after_open)) = rest.split_once(PARAMETER_OPEN) {
        let Some((key, after_key)) = after_open.split_once('>') else {
            break;
        };
        let Some((value, remainder)) = after_key.split_once(PARAMETER_CLOSE) else {
            break;
        };
        rest = remainder;
        if !key.is_empty() {
            // The newlines around a value are the format, not what was typed.
            arguments.push((key.to_string(), value.trim().to_string()));
        }
    }
    arguments
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::providers::TokenUsage;

    fn is_tool_use(block: &ContentBlock) -> bool {
        matches!(block, ContentBlock::ToolUse { .. })
    }

    /// Verbatim from a cortecs.ai reply: the model wrote the call as text
    /// instead of emitting it.
    const FRAMED_GREP: &str = "<tool_call>\n<function=grep>\n<parameter=-n>\ntrue\n</parameter>\n<parameter=output_mode>\ncontent\n</parameter>\n<parameter=pattern>\nplugin.Open(...)\n</parameter>\n</function>\n</tool_call>";

    /// Verbatim from a cortecs.ai reply: the model wrote the call into its
    /// reasoning instead of emitting it.
    const FRAMED_READ: &str = "<tool_call>\n<function=read_file>\n<parameter=path>\n/Users/mav/dev/lambda/README.md\n</parameter>\n</function>\n</tool_call>";

    fn grep_tool() -> Vec<ProviderToolDefinition> {
        vec![ProviderToolDefinition {
            name: "grep".into(),
            description: "Search files".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "-n": {"type": "boolean"},
                    "output_mode": {"type": "string"},
                    "pattern": {"type": "string"},
                }
            }),
        }]
    }

    /// `FRAMED_READ` naming another file, for the tests that need two blocks.
    fn framed_read(path: &str) -> String {
        FRAMED_READ.replace("/Users/mav/dev/lambda/README.md", path)
    }

    fn read_file_tool() -> Vec<ProviderToolDefinition> {
        vec![ProviderToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        }]
    }

    fn thinking(text: &str) -> ContentBlock {
        ContentBlock::Thinking {
            thinking: text.into(),
            signature: String::new(),
        }
    }

    /// Repair a reply, returning its blocks and the events it emitted.
    fn run(
        content: Vec<ContentBlock>,
        status: ResponseStatus,
        tools: &[ProviderToolDefinition],
    ) -> (Vec<ContentBlock>, Vec<StreamEvent>) {
        let (response, events) = run_response(content, status, tools);
        (response.content, events)
    }

    /// The same, keeping the whole reply for the tests that read its status.
    fn run_response(
        content: Vec<ContentBlock>,
        status: ResponseStatus,
        tools: &[ProviderToolDefinition],
    ) -> (ModelResponse, Vec<StreamEvent>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&seen);
        let sink: Arc<dyn Fn(StreamEvent) + Send + Sync> =
            Arc::new(move |event| collected.lock().unwrap().push(event));
        let mut response = ModelResponse {
            content,
            status,
            usage: TokenUsage::default(),
            model: "test".into(),
        };
        repair(&mut response, tools, &sink);
        let events = seen.lock().unwrap().clone();
        (response, events)
    }

    /// The calls `text` carried, for the tests that do not read the prose.
    fn framed_in(text: &str) -> Vec<FramedCall> {
        split_framed_calls(text).1
    }

    #[test]
    fn framed_block_parses_into_a_call() {
        let calls = framed_in(FRAMED_GREP);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "grep");
        assert_eq!(
            calls[0].arguments,
            vec![
                ("-n".to_string(), "true".to_string()),
                ("output_mode".to_string(), "content".to_string()),
                ("pattern".to_string(), "plugin.Open(...)".to_string()),
            ]
        );
    }

    #[test]
    fn bare_function_block_is_not_a_call() {
        // Without the frame the same text is a call the model wrote about, not
        // one it emitted, so promoting it would run what it only considered.
        let bare = FRAMED_GREP
            .replace(TOOL_CALL_OPEN, "")
            .replace(TOOL_CALL_CLOSE, "");
        assert!(framed_in(&bare).is_empty());
    }

    #[test]
    fn a_block_cut_off_before_its_close_stays_in_the_prose() {
        // Half a call is not a call, and the text is then all that is left of it.
        let truncated = "<tool_call>\n<function=read_file>\n<parameter=path>\n/Users/m";
        let (prose, calls) = split_framed_calls(truncated);
        assert!(calls.is_empty());
        assert_eq!(prose, truncated);
    }

    #[test]
    fn a_complete_block_parses_alongside_a_truncated_one() {
        let text = format!("{FRAMED_GREP}\n<tool_call>\n<function=grep>\n<parameter=pattern>\nFunction(...)\n</parameter>");
        assert_eq!(framed_in(&text).len(), 1);
    }

    #[test]
    fn a_framed_block_leaves_the_prose_around_it() {
        let text = format!("Let me just run 5 searches:\n{FRAMED_GREP}\nDone.");
        assert_eq!(
            split_framed_calls(&text).0,
            "Let me just run 5 searches:\n\nDone."
        );
    }

    #[test]
    fn a_name_that_is_not_an_identifier_is_not_a_call() {
        // Removing it too would lose what the model wrote, with no call to
        // show for it.
        let spaced = FRAMED_READ.replace("read_file", "read file");
        let (prose, calls) = split_framed_calls(&spaced);
        assert!(calls.is_empty());
        assert_eq!(prose, spaced);
    }

    #[test]
    fn a_promoted_call_types_its_arguments_against_the_tool_schema() {
        // `-n` is declared boolean, and a tool reading it with `as_bool()`
        // sees nothing when it arrives as the text the model typed.
        let (content, _) = run(
            vec![thinking(FRAMED_GREP)],
            ResponseStatus::EndTurn,
            &grep_tool(),
        );
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { input, .. })
                if *input == serde_json::json!({
                    "-n": true, "output_mode": "content", "pattern": "plugin.Open(...)"
                })
        ));
    }

    #[test]
    fn a_promoted_value_that_misses_its_declared_type_stays_text() {
        // The tool reports the real problem rather than a guess at what was meant.
        let framed = FRAMED_GREP.replace("\ntrue\n", "\nyes\n");
        let (content, _) = run(
            vec![thinking(&framed)],
            ResponseStatus::EndTurn,
            &grep_tool(),
        );
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { input, .. }) if input["-n"] == serde_json::json!("yes")
        ));
    }

    #[test]
    fn framed_call_promotes_when_the_reply_carries_none() {
        let (content, events) = run(
            vec![thinking(FRAMED_READ)],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { name, input, .. })
                if name == "read_file"
                    && *input == serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"})
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "read_file"
        ));
    }

    #[test]
    fn two_framed_calls_are_promoted_under_distinct_ids() {
        let both = format!(
            "{}\n{}",
            framed_read("/first.md"),
            framed_read("/second.md")
        );
        let (content, events) = run(
            vec![thinking(&both)],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        let ids: Vec<&str> = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["repaired_0", "repaired_1"]);
        assert!(matches!(
            &events[..],
            [
                StreamEvent::ToolCallRepaired { tool_name: first },
                StreamEvent::ToolCallRepaired { tool_name: second },
            ] if first == "read_file" && second == "read_file"
        ));
    }

    #[test]
    fn promoting_turns_the_reply_into_a_tool_use() {
        let (response, _) = run_response(
            vec![thinking(FRAMED_READ)],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        assert_eq!(response.status, ResponseStatus::ToolUse);
    }

    #[test]
    fn a_framed_call_beside_a_delivered_one_is_still_promoted() {
        // The endpoint delivered `read_file` and converted nothing for `grep`,
        // whose block is the only record of it.
        let (content, events) = run(
            vec![
                thinking(FRAMED_GREP),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/a.md"}),
                },
            ],
            ResponseStatus::ToolUse,
            &read_file_tool(),
        );
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { id, name, .. })
                if id == "repaired_0" && name == "grep"
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "grep"
        ));
    }

    #[test]
    fn a_framed_call_the_endpoint_answered_differently_is_declined() {
        // The endpoint delivered `read_file` for one path while the model wrote
        // a block for another. Nothing runs the block, and the prose no longer
        // shows it, so the event is all that is left of it.
        let (content, events) = run(
            vec![
                ContentBlock::Text {
                    text: framed_read("/wanted.md"),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/delivered.md"}),
                },
            ],
            ResponseStatus::ToolUse,
            &read_file_tool(),
        );
        assert_eq!(content.iter().filter(|b| is_tool_use(b)).count(), 1);
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { tool_name, reason }]
                if tool_name == "read_file" && *reason == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_call_the_endpoint_delivered_is_never_promoted_beside_itself() {
        // The model wrote the call it also emitted. Promoting would run it twice.
        let (content, events) = run(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"}),
                },
            ],
            ResponseStatus::ToolUse,
            &read_file_tool(),
        );
        assert_eq!(content.iter().filter(|b| is_tool_use(b)).count(), 1);
        assert!(events.is_empty());
    }

    #[test]
    fn a_call_delivered_empty_is_filled_from_its_framed_block() {
        let (content, events) = run(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
            &read_file_tool(),
        );
        assert!(matches!(
            &content[1],
            ContentBlock::ToolUse { input, .. }
                if *input == serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"})
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "read_file"
        ));
    }

    #[test]
    fn each_empty_call_is_filled_from_its_own_framed_block() {
        // Its own block is the one at its position among its same-named siblings.
        let both = format!(
            "{}\n{}",
            framed_read("/first.md"),
            framed_read("/second.md")
        );
        let (content, _) = run(
            vec![
                thinking(&both),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/first.md"}),
                },
                ContentBlock::ToolUse {
                    id: "call_2".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
            &read_file_tool(),
        );
        assert!(matches!(
            &content[2],
            ContentBlock::ToolUse { input, .. } if *input == serde_json::json!({"path": "/second.md"})
        ));
    }

    #[test]
    fn a_second_framed_call_with_nowhere_to_go_is_declined() {
        // Two blocks written for one delivered call: the first fills it, and
        // the second has no call of its own left to reach.
        let both = format!(
            "{}\n{}",
            framed_read("/first.md"),
            framed_read("/second.md")
        );
        let (content, events) = run(
            vec![
                thinking(&both),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
            &read_file_tool(),
        );
        assert_eq!(content.iter().filter(|block| is_tool_use(block)).count(), 1);
        assert!(matches!(
            &events[..],
            [
                StreamEvent::ToolCallRepaired { .. },
                StreamEvent::ToolCallDeclined { reason, .. },
            ] if *reason == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_reply_carrying_no_framed_call_keeps_its_own_whitespace() {
        // Every reply on this path is scanned, so one with nothing to repair
        // must come back exactly as the model wrote it.
        let written = "  an answer, indented on purpose  ";
        let (content, events) = run(
            vec![ContentBlock::Text {
                text: written.into(),
            }],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }] if text == written
        ));
        assert!(events.is_empty());
    }

    #[test]
    fn a_delivered_call_with_arguments_is_left_alone() {
        // Overwriting one would run the endpoint's call with the framed block's
        // arguments; adding one would run the same call twice.
        let delivered = serde_json::json!({"path": "/other.md"});
        let (content, events) = run(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: delivered.clone(),
                },
            ],
            ResponseStatus::ToolUse,
            &read_file_tool(),
        );
        assert_eq!(content.len(), 2);
        assert!(matches!(&content[1], ContentBlock::ToolUse { input, .. } if *input == delivered));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { reason, .. }]
                if *reason == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_truncated_reply_declines_instead_of_promoting() {
        let (content, events) = run(
            vec![thinking(FRAMED_READ)],
            ResponseStatus::OutputTruncated,
            &read_file_tool(),
        );
        assert!(!content.iter().any(is_tool_use));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { reason, .. }]
                if *reason == ToolDeclineKind::OutputTruncated
        ));
    }

    #[test]
    fn a_refused_reply_declines_as_not_finished() {
        // Apart from a truncated one: nothing was cut off, the model stopped
        // before committing to the call it had already written.
        let (content, events) = run(
            vec![thinking(FRAMED_READ)],
            ResponseStatus::Refused,
            &read_file_tool(),
        );
        assert!(!content.iter().any(is_tool_use));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { reason, .. }]
                if *reason == ToolDeclineKind::ReplyNotFinished
        ));
    }

    #[test]
    fn an_unknown_name_is_promoted_with_its_arguments_untyped() {
        // No schema names a type, so every value stays the text the model typed.
        let (content, events) = run(vec![thinking(FRAMED_GREP)], ResponseStatus::EndTurn, &[]);
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { name, input, .. })
                if name == "grep"
                    && *input == serde_json::json!({
                        "-n": "true", "output_mode": "content", "pattern": "plugin.Open(...)"
                    })
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "grep"
        ));
    }

    #[test]
    fn a_declined_call_keeps_the_text_the_model_wrote() {
        // Nothing ran, so nothing is taken away: stripping the syntax off a
        // call that will never run would leave no record of it anywhere.
        let written = format!("Reading it now.\n{FRAMED_READ}");
        let (content, _) = run(
            vec![ContentBlock::Text {
                text: written.clone(),
            }],
            ResponseStatus::OutputTruncated,
            &read_file_tool(),
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }] if *text == written
        ));
    }

    #[test]
    fn a_call_written_in_the_reply_runs_and_leaves_its_prose() {
        // The commonest shape: the model wrote the call where it writes to be
        // read, so the call has to run and its syntax has to go.
        let (content, events) = run(
            vec![ContentBlock::Text {
                text: format!("Reading it now.\n{FRAMED_READ}"),
            }],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }, ContentBlock::ToolUse { name, .. }]
                if text == "Reading it now." && name == "read_file"
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "read_file"
        ));
    }

    #[test]
    fn a_reply_without_framed_calls_is_untouched() {
        let answer = "Just an answer.";
        let (content, events) = run(
            vec![ContentBlock::Text {
                text: answer.into(),
            }],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }] if text == answer
        ));
        assert!(events.is_empty());
    }
}

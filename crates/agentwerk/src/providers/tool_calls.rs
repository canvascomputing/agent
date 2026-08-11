//! Reads back the tool calls a model wrote as text rather than emitting through
//! the tool channel, and clears the leftover syntax from the reply.
//!
//! These models are trained to write a call as
//! `<tool_call><function=NAME><parameter=KEY>VALUE</parameter>…</function></tool_call>`
//! for the endpoint to convert. They do not do it consistently: in one run the
//! same model emits some calls properly and writes others as text, and it also
//! writes a call it did emit. A call left as text costs the turn — the loop
//! answers "no tool call", or the tool answers "missing required parameter" for
//! one delivered empty. What the model wrote is still in the reply, so the call
//! is read from there.

use std::collections::HashMap;
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
/// accord and every framed call names a tool this request advertised.
pub(crate) fn repair(
    response: &mut ModelResponse,
    tools: &[ProviderToolDefinition],
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let framed: Vec<FramedCall> = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
            _ => None,
        })
        .flat_map(parse_framed_calls)
        .collect();

    if let Some(reason) = decline_reason(response, &framed, tools) {
        for call in &framed {
            on_event(StreamEvent::ToolCallDeclined {
                tool_name: call.name.clone(),
                reason,
            });
        }
    } else if response.content.iter().any(is_tool_use) {
        fill_empty_calls(response, tools, &framed, on_event);
    } else {
        promote_framed_calls(response, tools, &framed, on_event);
    }

    for block in &mut response.content {
        if let ContentBlock::Text { text } = block {
            *text = strip_framed_calls(text);
        }
    }
    response
        .content
        .retain(|block| !matches!(block, ContentBlock::Text { text } if text.is_empty()));
}

fn decline_reason(
    response: &ModelResponse,
    framed: &[FramedCall],
    tools: &[ProviderToolDefinition],
) -> Option<ToolDeclineKind> {
    if framed.is_empty() {
        return None;
    }
    // A length or filter terminal can end a reply just past a complete
    // `</function>`, leaving whole-looking syntax the model never committed to.
    if !matches!(
        response.status,
        ResponseStatus::EndTurn | ResponseStatus::ToolUse
    ) {
        return Some(ToolDeclineKind::OutputTruncated);
    }
    let all_advertised = framed
        .iter()
        .all(|call| definition_for(tools, &call.name).is_some());
    (!all_advertised).then_some(ToolDeclineKind::ToolNotAdvertised)
}

/// Add each framed call to a reply that carried none.
fn promote_framed_calls(
    response: &mut ModelResponse,
    tools: &[ProviderToolDefinition],
    framed: &[FramedCall],
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    for (offset, call) in framed.iter().enumerate() {
        let Some(definition) = definition_for(tools, &call.name) else {
            continue;
        };
        response.content.push(ContentBlock::ToolUse {
            id: format!("recovered_{offset}"),
            name: call.name.clone(),
            input: typed_arguments(call, &definition.input_schema),
        });
        on_event(StreamEvent::ToolCallRecovered {
            tool_name: call.name.clone(),
        });
    }
    if !framed.is_empty() {
        response.status = ResponseStatus::ToolUse;
    }
}

/// Complete the calls the endpoint delivered empty, each from the framed block
/// at the same position among its same-named siblings. Never adds a call, so a
/// call the endpoint delivered cannot run twice.
fn fill_empty_calls(
    response: &mut ModelResponse,
    tools: &[ProviderToolDefinition],
    framed: &[FramedCall],
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for block in &mut response.content {
        let ContentBlock::ToolUse { name, input, .. } = block else {
            continue;
        };
        let offset = seen.entry(name.as_str()).or_default();
        let candidate = framed.iter().filter(|call| &call.name == name).nth(*offset);
        *offset += 1;
        let empty = input.as_object().is_none_or(|fields| fields.is_empty());
        let Some((call, definition)) = candidate.zip(definition_for(tools, name)) else {
            continue;
        };
        if !empty {
            continue;
        }
        *input = typed_arguments(call, &definition.input_schema);
        on_event(StreamEvent::ToolCallRecovered {
            tool_name: name.clone(),
        });
    }
}

fn is_tool_use(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::ToolUse { .. })
}

fn definition_for<'a>(
    tools: &'a [ProviderToolDefinition],
    name: &str,
) -> Option<&'a ProviderToolDefinition> {
    tools.iter().find(|tool| tool.name == name)
}

/// A tool call the model wrote as text instead of emitting it. Every value is
/// the text the model typed; `typed_arguments` retypes them against the tool's
/// own schema.
struct FramedCall {
    name: String,
    arguments: Vec<(String, String)>,
}

/// Read every `<tool_call><function=NAME><parameter=KEY>VALUE</parameter>…
/// </function></tool_call>` block out of `text`, in the order they appear.
///
/// The `<tool_call>` frame is required. It is the model's own syntax for making
/// a call, which is what separates one from a call it merely wrote about. A
/// block missing any closing tag is skipped: half a call is not a call.
fn parse_framed_calls(text: &str) -> Vec<FramedCall> {
    let mut calls = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find(TOOL_CALL_OPEN) {
        let framed = &rest[open + TOOL_CALL_OPEN.len()..];
        let Some(close) = framed.find(TOOL_CALL_CLOSE) else {
            break;
        };
        rest = &framed[close + TOOL_CALL_CLOSE.len()..];
        if let Some(call) = parse_function_block(&framed[..close]) {
            calls.push(call);
        }
    }
    calls
}

/// Remove the framed blocks, leaving the prose the model meant to be read.
fn strip_framed_calls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(TOOL_CALL_OPEN) {
        let framed = &rest[open + TOOL_CALL_OPEN.len()..];
        let Some(close) = framed.find(TOOL_CALL_CLOSE) else {
            break;
        };
        out.push_str(&rest[..open]);
        rest = &framed[close + TOOL_CALL_CLOSE.len()..];
    }
    out.push_str(rest);
    out.trim().to_string()
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

fn parse_function_block(body: &str) -> Option<FramedCall> {
    let open = body.find(FUNCTION_OPEN)?;
    let after_open = &body[open + FUNCTION_OPEN.len()..];
    let name_end = after_open.find('>')?;
    let name = &after_open[..name_end];
    let inner = &after_open[name_end + 1..];
    let close = inner.find(FUNCTION_CLOSE)?;
    if !is_identifier(name) {
        return None;
    }
    Some(FramedCall {
        name: name.to_string(),
        arguments: parse_parameters(&inner[..close]),
    })
}

fn parse_parameters(body: &str) -> Vec<(String, String)> {
    let mut arguments = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find(PARAMETER_OPEN) {
        let after_open = &rest[open + PARAMETER_OPEN.len()..];
        let Some(key_end) = after_open.find('>') else {
            break;
        };
        let key = &after_open[..key_end];
        let value = &after_open[key_end + 1..];
        let Some(value_end) = value.find(PARAMETER_CLOSE) else {
            break;
        };
        rest = &value[value_end + PARAMETER_CLOSE.len()..];
        if !key.is_empty() {
            // The newlines around a value are the format, not what was typed.
            arguments.push((key.to_string(), value[..value_end].trim().to_string()));
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

    /// Verbatim from a cortecs.ai reply: the model wrote the call as text
    /// instead of emitting it.
    const FRAMED_GREP: &str = "<tool_call>\n<function=grep>\n<parameter=-n>\ntrue\n</parameter>\n<parameter=output_mode>\ncontent\n</parameter>\n<parameter=pattern>\nplugin.Open(...)\n</parameter>\n</function>\n</tool_call>";

    /// Verbatim from a cortecs.ai reply: the model wrote the call into its
    /// reasoning instead of emitting it.
    const FRAMED_READ: &str = "<tool_call>\n<function=read_file>\n<parameter=path>\n/Users/mav/dev/lambda/README.md\n</parameter>\n</function>\n</tool_call>";

    fn grep_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "-n": {"type": "boolean"},
                "output_mode": {"type": "string"},
                "pattern": {"type": "string"},
            }
        })
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

    /// Recover over a reply, returning its blocks and the events it emitted.
    fn run(
        content: Vec<ContentBlock>,
        status: ResponseStatus,
        tools: &[ProviderToolDefinition],
    ) -> (Vec<ContentBlock>, Vec<StreamEvent>) {
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
        (response.content, events)
    }

    #[test]
    fn framed_block_parses_into_a_call() {
        let calls = parse_framed_calls(FRAMED_GREP);
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
        assert!(parse_framed_calls(&bare).is_empty());
    }

    #[test]
    fn block_cut_off_before_its_close_is_skipped() {
        let truncated = "<tool_call>\n<function=read_file>\n<parameter=path>\n/Users/m";
        assert!(parse_framed_calls(truncated).is_empty());
    }

    #[test]
    fn complete_blocks_parse_alongside_a_truncated_one() {
        let text = format!("{FRAMED_GREP}\n<tool_call>\n<function=grep>\n<parameter=pattern>\nFunction(...)\n</parameter>");
        assert_eq!(parse_framed_calls(&text).len(), 1);
    }

    #[test]
    fn strip_removes_the_frames_and_leaves_the_prose() {
        let text = format!("Let me just run 5 searches:\n{FRAMED_GREP}\nDone.");
        assert_eq!(
            strip_framed_calls(&text),
            "Let me just run 5 searches:\n\nDone."
        );
    }

    #[test]
    fn arguments_are_typed_against_the_tool_schema() {
        let calls = parse_framed_calls(FRAMED_GREP);
        assert_eq!(
            typed_arguments(&calls[0], &grep_schema()),
            serde_json::json!({"-n": true, "output_mode": "content", "pattern": "plugin.Open(...)"})
        );
    }

    #[test]
    fn a_value_that_misses_its_declared_type_stays_text() {
        let call = FramedCall {
            name: "grep".into(),
            arguments: vec![("-n".into(), "yes".into())],
        };
        assert_eq!(
            typed_arguments(&call, &grep_schema()),
            serde_json::json!({"-n": "yes"})
        );
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
            [StreamEvent::ToolCallRecovered { tool_name }] if tool_name == "read_file"
        ));
    }

    #[test]
    fn empty_native_call_is_filled_from_its_framed_block() {
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
        assert_eq!(events.len(), 1);
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
        assert!(events.is_empty());
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
    fn an_unadvertised_name_declines_every_framed_call() {
        let (content, events) = run(vec![thinking(FRAMED_READ)], ResponseStatus::EndTurn, &[]);
        assert!(!content.iter().any(is_tool_use));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { reason, .. }]
                if *reason == ToolDeclineKind::ToolNotAdvertised
        ));
    }

    #[test]
    fn framed_syntax_is_stripped_from_the_visible_text() {
        let (content, _) = run(
            vec![ContentBlock::Text {
                text: format!("Reading it now.\n{FRAMED_READ}"),
            }],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        assert!(matches!(
            &content[0],
            ContentBlock::Text { text } if text == "Reading it now."
        ));
    }

    #[test]
    fn a_reply_without_framed_calls_is_untouched() {
        let (content, events) = run(
            vec![ContentBlock::Text {
                text: "Just an answer.".into(),
            }],
            ResponseStatus::EndTurn,
            &read_file_tool(),
        );
        assert_eq!(content.len(), 1);
        assert!(events.is_empty());
    }
}

//! The tool calls a model wrote as text, read back as calls it can run.
//!
//! Some models write a call as `<tool_call><function=NAME>…</function></tool_call>`
//! for the endpoint to convert, and do it inconsistently within one run. One
//! left as text costs the turn: the loop answers "no tool call", or the tool
//! answers "missing required parameter" for one delivered empty.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use serde_json::{Map, Value};

use super::types::{
    ContentBlock, Message, ModelRequest, ModelResponse, ResponseStatus, StreamEvent,
    ToolDeclineKind,
};
use crate::tools::Tool;

const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";
const FUNCTION_OPEN: &str = "<function=";
const FUNCTION_CLOSE: &str = "</function>";
const PARAMETER_OPEN: &str = "<parameter=";
const PARAMETER_CLOSE: &str = "</parameter>";

/// One transactional pass over the calls a model wrote as text.
pub(crate) struct FrameRecovery<'a> {
    request: &'a ModelRequest,
    response: &'a mut ModelResponse,
    on_event: &'a Arc<dyn Fn(StreamEvent) + Send + Sync>,
    calls: Vec<FramedCall>,
    native_calls: Vec<NativeCall>,
    native_positions: HashMap<String, Vec<usize>>,
    used_call_ids: HashSet<String>,
    next_call_id: usize,
    applied_frames: AppliedFrames,
    input_updates: Vec<(usize, Value)>,
    added_calls: Vec<ContentBlock>,
}

impl<'a> FrameRecovery<'a> {
    pub(crate) fn new(
        request: &'a ModelRequest,
        response: &'a mut ModelResponse,
        on_event: &'a Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Self {
        Self {
            request,
            response,
            on_event,
            calls: Vec::new(),
            native_calls: Vec::new(),
            native_positions: HashMap::new(),
            used_call_ids: HashSet::new(),
            next_call_id: 0,
            applied_frames: AppliedFrames::default(),
            input_updates: Vec::new(),
            added_calls: Vec::new(),
        }
    }

    /// Recover complete calls only after the model finished the reply itself.
    pub(crate) fn recover_response(mut self) {
        self.parse_calls();
        if self.calls.is_empty() {
            return;
        }
        if let Some(reason) = Self::determine_decline_reason(&self.response.status) {
            for call in &self.calls {
                self.emit_decline(call, reason);
            }
            return;
        }

        self.index_native_calls();
        self.collect_call_ids();
        self.reconcile_calls();
        self.commit_inputs();
        self.remove_frames();
        self.commit_status();
    }

    /// Read every framed call while leaving the response unchanged.
    fn parse_calls(&mut self) {
        for (content_index, block) in self.response.content.iter().enumerate() {
            // Thinking is read but never stripped: the endpoint takes the block
            // back verbatim.
            let (source, text) = match block {
                ContentBlock::Text { text } => (FrameSource::Text { content_index }, text),
                ContentBlock::Thinking { thinking, .. } => (FrameSource::Thinking, thinking),
                _ => continue,
            };
            if !text.contains(TOOL_CALL_OPEN) {
                continue;
            }

            let mut cursor = 0;
            while let Some(relative_start) = text[cursor..].find(TOOL_CALL_OPEN) {
                let start = cursor + relative_start;
                let body_start = start + TOOL_CALL_OPEN.len();
                let next_open = text[body_start..]
                    .find(TOOL_CALL_OPEN)
                    .map(|offset| body_start + offset);
                let Some(body_end) = text[body_start..]
                    .find(TOOL_CALL_CLOSE)
                    .map(|offset| body_start + offset)
                else {
                    break;
                };
                if next_open.is_some_and(|nested| nested < body_end) {
                    cursor = next_open.expect("checked as present");
                    continue;
                }
                let end = body_end + TOOL_CALL_CLOSE.len();
                if let Some(call) =
                    FramedCall::parse_frame(source, start..end, &text[body_start..body_end])
                {
                    self.calls.push(call);
                }
                cursor = end;
            }
        }
    }

    fn index_native_calls(&mut self) {
        self.native_calls = self
            .response
            .content
            .iter()
            .enumerate()
            .filter_map(|(content_index, block)| match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => Some(NativeCall {
                    content_index,
                    id: id.clone(),
                    identity: self.resolve_tool_identity(name),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect();
        for (index, call) in self.native_calls.iter().enumerate() {
            self.native_positions
                .entry(call.identity.clone())
                .or_default()
                .push(index);
        }
    }

    fn collect_call_ids(&mut self) {
        for message in &self.request.messages {
            let blocks = match message {
                Message::User { content } | Message::Assistant { content } => content,
                Message::System { .. } => continue,
            };
            for block in blocks {
                match block {
                    ContentBlock::ToolUse { id, .. } => {
                        self.used_call_ids.insert(id.clone());
                    }
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        self.used_call_ids.insert(tool_use_id.clone());
                    }
                    _ => {}
                }
            }
        }
        self.used_call_ids
            .extend(self.native_calls.iter().map(|call| call.id.clone()));
    }

    /// Match calls by position among same-named siblings to avoid running a
    /// native call twice while still recovering a separate framed call.
    fn reconcile_calls(&mut self) {
        let mut seen: HashMap<String, usize> = HashMap::new();
        for call in std::mem::take(&mut self.calls) {
            let identity = self.resolve_tool_identity(&call.name);
            let position = seen.entry(identity.clone()).or_default();
            let at = *position;
            *position += 1;

            let Some(native_index) = self
                .native_positions
                .get(&identity)
                .and_then(|matches| matches.get(at))
                .copied()
            else {
                if self.native_positions.contains_key(&identity) {
                    self.emit_decline(&call, ToolDeclineKind::AlreadyDelivered);
                } else {
                    let call_id = self.allocate_call_id();
                    self.added_calls.push(ContentBlock::ToolUse {
                        id: call_id.clone(),
                        name: call.name.clone(),
                        input: call.build_input(),
                    });
                    self.emit_repair(&call, call_id);
                    self.applied_frames.record_frame(&call);
                }
                continue;
            };

            let native = &self.native_calls[native_index];
            if native.input.as_object().is_some_and(Map::is_empty) {
                self.input_updates
                    .push((native.content_index, call.build_input()));
                self.emit_repair(&call, native.id.clone());
                self.applied_frames.record_frame(&call);
            } else if call.matches_native_input(&native.input) {
                self.applied_frames.record_frame(&call);
            } else {
                self.emit_decline(&call, ToolDeclineKind::AlreadyDelivered);
            }
        }
    }

    fn commit_inputs(&mut self) {
        for (content_index, repaired_input) in std::mem::take(&mut self.input_updates) {
            let ContentBlock::ToolUse { input, .. } = &mut self.response.content[content_index]
            else {
                unreachable!("native call index still names a tool call");
            };
            *input = repaired_input;
        }
    }

    fn remove_frames(&mut self) {
        self.applied_frames.rebuild_response(self.response);
    }

    fn commit_status(&mut self) {
        if self.added_calls.is_empty() {
            return;
        }
        self.response.content.append(&mut self.added_calls);
        // Without the flip the reply reads as an answer and nothing added runs.
        self.response.status = ResponseStatus::ToolUse;
    }

    fn allocate_call_id(&mut self) -> String {
        loop {
            let id = format!("repaired_{}", self.next_call_id);
            self.next_call_id += 1;
            if self.used_call_ids.insert(id.clone()) {
                return id;
            }
        }
    }

    fn resolve_tool_identity(&self, name: &str) -> String {
        Tool::find_tool(&self.request.tools, name)
            .map(|tool| tool.get_name())
            .unwrap_or(name)
            .to_string()
    }

    fn determine_decline_reason(status: &ResponseStatus) -> Option<ToolDeclineKind> {
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

    fn emit_decline(&self, call: &FramedCall, kind: ToolDeclineKind) {
        (self.on_event)(StreamEvent::ToolCallDeclined {
            tool_name: call.name.clone(),
            kind,
        });
    }

    fn emit_repair(&self, call: &FramedCall, call_id: impl Into<String>) {
        (self.on_event)(StreamEvent::ToolCallRepaired {
            tool_name: call.name.clone(),
            call_id: call_id.into(),
        });
    }
}

struct NativeCall {
    content_index: usize,
    id: String,
    identity: String,
    input: Value,
}

#[derive(Clone, Copy)]
enum FrameSource {
    Text { content_index: usize },
    Thinking,
}

#[derive(Default)]
struct AppliedFrames {
    spans: HashMap<usize, Vec<Range<usize>>>,
}

impl AppliedFrames {
    fn record_frame(&mut self, call: &FramedCall) {
        let FrameSource::Text { content_index } = call.source else {
            return;
        };
        self.spans
            .entry(content_index)
            .or_default()
            .push(call.span.clone());
    }

    fn rebuild_response(&mut self, response: &mut ModelResponse) {
        for spans in self.spans.values_mut() {
            spans.sort_by_key(|span| span.start);
        }
        for (content_index, block) in response.content.iter_mut().enumerate() {
            let ContentBlock::Text { text } = block else {
                continue;
            };
            let Some(spans) = self.spans.get(&content_index) else {
                continue;
            };
            let mut kept = String::with_capacity(text.len());
            let mut after = 0;
            for span in spans {
                kept.push_str(&text[after..span.start]);
                after = span.end;
            }
            kept.push_str(&text[after..]);
            *text = kept;
        }
        response
            .content
            .retain(|block| !matches!(block, ContentBlock::Text { text } if text.is_empty()));
    }
}

/// A tool call the model wrote as text instead of emitting it; every value is
/// the text the model typed.
struct FramedCall {
    source: FrameSource,
    span: Range<usize>,
    name: String,
    arguments: Vec<(String, String)>,
}

impl FramedCall {
    fn parse_frame(source: FrameSource, span: Range<usize>, body: &str) -> Option<Self> {
        let after_open = body.trim_start().strip_prefix(FUNCTION_OPEN)?;
        let (name, inner) = after_open.split_once('>')?;
        let (parameters, trailing) = inner.split_once(FUNCTION_CLOSE)?;
        if !trailing.trim().is_empty() || !Self::validate_tool_name(name) {
            return None;
        }
        Some(Self {
            source,
            span,
            name: name.to_string(),
            arguments: Self::parse_parameters(parameters)?,
        })
    }

    fn parse_parameters(mut body: &str) -> Option<Vec<(String, String)>> {
        let mut arguments = Vec::new();
        let mut names = HashSet::new();
        loop {
            body = body.trim_start_matches(char::is_whitespace);
            if body.is_empty() {
                return Some(arguments);
            }
            let after_open = body.strip_prefix(PARAMETER_OPEN)?;
            let (name, after_name) = after_open.split_once('>')?;
            if name.is_empty() || !names.insert(name) {
                return None;
            }
            let (value, remainder) = after_name.split_once(PARAMETER_CLOSE)?;
            arguments.push((
                name.to_string(),
                Self::remove_layout_newline(value).to_string(),
            ));
            body = remainder;
        }
    }

    /// Leave values as text for schema validation to retype against the tool.
    fn build_input(&self) -> Value {
        let fields = self
            .arguments
            .iter()
            .map(|(name, text)| (name.clone(), Value::String(text.clone())));
        Value::Object(Map::from_iter(fields))
    }

    /// Compare text values by what JSON reads them as when the native call is typed.
    fn matches_native_input(&self, delivered: &Value) -> bool {
        let Some(fields) = delivered.as_object() else {
            return false;
        };
        fields.len() == self.arguments.len()
            && self.arguments.iter().all(|(name, text)| {
                fields.get(name).is_some_and(|value| match value {
                    Value::String(delivered) => delivered == text,
                    other => serde_json::from_str::<Value>(text).is_ok_and(|read| read == *other),
                })
            })
    }

    fn remove_layout_newline(mut value: &str) -> &str {
        value = value
            .strip_prefix("\r\n")
            .or_else(|| value.strip_prefix('\n'))
            .unwrap_or(value);
        value
            .strip_suffix("\r\n")
            .or_else(|| value.strip_suffix('\n'))
            .unwrap_or(value)
    }

    fn validate_tool_name(name: &str) -> bool {
        !name.is_empty()
            && name.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '_' || character == '-'
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::providers::{ReasoningEffort, TokenUsage};
    use crate::tools::Tool;

    fn is_tool_use(block: &ContentBlock) -> bool {
        matches!(block, ContentBlock::ToolUse { .. })
    }

    /// Verbatim from a cortecs.ai reply: the model wrote the call as text
    /// instead of emitting it.
    const FRAMED_GREP: &str = "<tool_call>\n<function=grep>\n<parameter=-n>\ntrue\n</parameter>\n<parameter=output_mode>\ncontent\n</parameter>\n<parameter=pattern>\nplugin.Open(...)\n</parameter>\n</function>\n</tool_call>";

    /// Verbatim from a cortecs.ai reply: the model wrote the call into its
    /// reasoning instead of emitting it.
    const FRAMED_READ: &str = "<tool_call>\n<function=read_file>\n<parameter=path>\n/Users/mav/dev/lambda/README.md\n</parameter>\n</function>\n</tool_call>";

    /// `FRAMED_READ` naming another file, for the tests that need two blocks.
    fn framed_read(path: &str) -> String {
        FRAMED_READ.replace("/Users/mav/dev/lambda/README.md", path)
    }

    fn thinking(text: &str) -> ContentBlock {
        ContentBlock::Thinking {
            thinking: text.into(),
            signature: String::new(),
        }
    }

    fn recover(
        content: Vec<ContentBlock>,
        status: ResponseStatus,
    ) -> (Vec<ContentBlock>, Vec<StreamEvent>) {
        let (response, events) = recover_response(request(&[], Vec::new()), content, status);
        (response.content, events)
    }

    fn recover_response(
        request: ModelRequest,
        content: Vec<ContentBlock>,
        status: ResponseStatus,
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
        FrameRecovery::new(&request, &mut response, &sink).recover_response();
        let events = seen.lock().unwrap().clone();
        (response, events)
    }

    fn request(tools: &[&str], messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "test".into(),
            system_prompt: String::new(),
            messages,
            tools: tools.iter().map(|name| Tool::new(*name)).collect(),
            max_request_tokens: None,
            reasoning_effort: ReasoningEffort::Off,
        }
    }

    #[test]
    fn a_committed_frame_becomes_one_tool_call() {
        let (response, events) = recover_response(
            request(&[], Vec::new()),
            vec![thinking(FRAMED_READ)],
            ResponseStatus::EndTurn,
        );
        assert_eq!(response.status, ResponseStatus::ToolUse);
        assert!(matches!(
            response.content.last(),
            Some(ContentBlock::ToolUse { name, input, .. })
                if name == "read_file"
                    && *input == serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"})
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name, call_id }]
                if tool_name == "read_file" && call_id == "repaired_0"
        ));
    }

    #[test]
    fn two_framed_calls_are_promoted_under_distinct_ids() {
        let both = format!(
            "{}\n{}",
            framed_read("/first.md"),
            framed_read("/second.md")
        );
        let (content, events) = recover(vec![thinking(&both)], ResponseStatus::EndTurn);
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
                StreamEvent::ToolCallRepaired { tool_name: first, call_id: first_id },
                StreamEvent::ToolCallRepaired { tool_name: second, call_id: second_id },
            ] if first == "read_file" && second == "read_file"
                && first_id == "repaired_0" && second_id == "repaired_1"
        ));
    }

    #[test]
    fn a_framed_call_beside_a_delivered_one_is_still_promoted() {
        // The endpoint delivered `read_file` and converted nothing for `grep`,
        // whose block is the only record of it.
        let (content, events) = recover(
            vec![
                thinking(FRAMED_GREP),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/a.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { id, name, .. })
                if id == "repaired_0" && name == "grep"
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name, call_id }]
                if tool_name == "grep" && call_id == "repaired_0"
        ));
    }

    #[test]
    fn a_framed_call_the_endpoint_answered_differently_is_declined() {
        let written = framed_read("/wanted.md");
        let (content, events) = recover(
            vec![
                ContentBlock::Text {
                    text: written.clone(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/delivered.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert_eq!(content.iter().filter(|b| is_tool_use(b)).count(), 1);
        assert!(matches!(&content[0], ContentBlock::Text { text } if *text == written));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { tool_name, kind }]
                if tool_name == "read_file" && *kind == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_call_the_endpoint_delivered_is_never_promoted_beside_itself() {
        // The model wrote the call it also emitted. Promoting would run it twice.
        let (content, events) = recover(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert_eq!(content.iter().filter(|b| is_tool_use(b)).count(), 1);
        assert!(events.is_empty());
    }

    #[test]
    fn a_call_delivered_empty_is_filled_from_its_framed_block() {
        let (content, events) = recover(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert!(matches!(
            &content[1],
            ContentBlock::ToolUse { input, .. }
                if *input == serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"})
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name, call_id }]
                if tool_name == "read_file" && call_id == "call_1"
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
        let (content, _) = recover(
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
        let (content, events) = recover(
            vec![
                thinking(&both),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert_eq!(content.iter().filter(|block| is_tool_use(block)).count(), 1);
        assert!(matches!(
            &events[..],
            [
                StreamEvent::ToolCallRepaired { .. },
                StreamEvent::ToolCallDeclined { kind, .. },
            ] if *kind == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_delivered_call_the_model_also_framed_is_one_call() {
        // A framed value is text and a delivered one is typed, so comparing
        // them as they stand would report the same call as two.
        let framed = "<tool_call>\n<function=read_file>\n<parameter=offset>\n100\n</parameter>\n</function>\n</tool_call>";
        let (_, events) = recover(
            vec![
                thinking(framed),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"offset": 100}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_native_call_and_its_framed_alias_remain_one_call() {
        let (response, events) = recover_response(
            request(&["read_file"], Vec::new()),
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file_tool".into(),
                    input: serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );

        assert_eq!(
            response
                .content
                .iter()
                .filter(|block| is_tool_use(block))
                .count(),
            1
        );
        assert!(events.is_empty());
    }

    #[test]
    fn a_framed_alias_fills_an_empty_native_call() {
        let (response, events) = recover_response(
            request(&["read_file"], Vec::new()),
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file_tool".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
        );

        assert!(matches!(
            &response.content[1],
            ContentBlock::ToolUse { input, .. }
                if *input == serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"})
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { call_id, .. }] if call_id == "call_1"
        ));
    }

    #[test]
    fn distinct_exact_tool_names_are_not_deduplicated() {
        let (response, _) = recover_response(
            request(&["read_file", "read_file_tool"], Vec::new()),
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file_tool".into(),
                    input: serde_json::json!({"path": "/other.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );

        assert_eq!(
            response
                .content
                .iter()
                .filter(|block| is_tool_use(block))
                .count(),
            2
        );
    }

    #[test]
    fn repaired_ids_skip_current_and_historical_collisions() {
        let history = vec![
            Message::Assistant {
                content: vec![ContentBlock::ToolUse {
                    id: "repaired_0".into(),
                    name: "grep".into(),
                    input: serde_json::json!({}),
                }],
            },
            Message::User {
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "repaired_0".into(),
                    content: "done".into(),
                    succeeded: true,
                }],
            },
        ];
        let (response, events) = recover_response(
            request(&[], history),
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "repaired_1".into(),
                    name: "grep".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
        );

        assert!(matches!(
            response.content.last(),
            Some(ContentBlock::ToolUse { id, name, .. })
                if id == "repaired_2" && name == "read_file"
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { call_id, .. }] if call_id == "repaired_2"
        ));
    }

    #[test]
    fn malformed_frames_remain_text_and_produce_no_call() {
        let cases = [
            (
                "incomplete parameter",
                "<tool_call><function=read_file><parameter=path>/a</function></tool_call>",
            ),
            (
                "duplicate parameter",
                "<tool_call><function=read_file><parameter=path>/a</parameter><parameter=path>/b</parameter></function></tool_call>",
            ),
            (
                "trailing syntax",
                "<tool_call><function=read_file></function>trailing</tool_call>",
            ),
            (
                "second function",
                "<tool_call><function=read_file></function><function=grep></function></tool_call>",
            ),
        ];

        for (case, written) in cases {
            let (content, events) = recover(
                vec![ContentBlock::Text {
                    text: written.into(),
                }],
                ResponseStatus::EndTurn,
            );
            assert!(
                matches!(
                    &content[..],
                    [ContentBlock::Text { text }] if text == written
                ),
                "{case}"
            );
            assert!(events.is_empty(), "{case}: {events:?}");
        }
    }

    #[test]
    fn a_valid_frame_survives_a_separate_malformed_frame() {
        let malformed = "<tool_call><function=read_file><parameter=path>/unfinished";
        let written = format!("{FRAMED_READ}\n{malformed}");
        let (content, events) = recover(
            vec![ContentBlock::Text { text: written }],
            ResponseStatus::EndTurn,
        );

        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }, ContentBlock::ToolUse { .. }]
                if text == &format!("\n{malformed}")
        ));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parameter_layout_is_removed_without_trimming_its_value() {
        let framed = "<tool_call>\r\n<function=echo>\r\n<parameter=text>\r\n  first\nsecond  \r\n</parameter>\r\n</function>\r\n</tool_call>";
        let (content, _) = recover(vec![thinking(framed)], ResponseStatus::EndTurn);

        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { input, .. })
                if *input == serde_json::json!({"text": "  first\nsecond  "})
        ));
    }

    #[test]
    fn nonempty_native_input_is_never_replaced() {
        let written = FRAMED_READ.to_string();
        let (content, events) = recover(
            vec![
                ContentBlock::Text {
                    text: written.clone(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: Value::String("encoded arguments".into()),
                },
            ],
            ResponseStatus::ToolUse,
        );

        assert!(matches!(&content[0], ContentBlock::Text { text } if *text == written));
        assert!(
            matches!(&content[1], ContentBlock::ToolUse { input, .. } if input == "encoded arguments")
        );
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { kind, .. }]
                if *kind == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_truncated_reply_declines_instead_of_promoting() {
        let (content, events) =
            recover(vec![thinking(FRAMED_READ)], ResponseStatus::OutputTruncated);
        assert!(!content.iter().any(is_tool_use));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { kind, .. }]
                if *kind == ToolDeclineKind::OutputTruncated
        ));
    }

    #[test]
    fn a_refused_reply_declines_as_not_finished() {
        // Apart from a truncated one: nothing was cut off, the model stopped
        // before committing to the call it had already written.
        let (content, events) = recover(vec![thinking(FRAMED_READ)], ResponseStatus::Refused);
        assert!(!content.iter().any(is_tool_use));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { kind, .. }]
                if *kind == ToolDeclineKind::ReplyNotFinished
        ));
    }

    #[test]
    fn a_declined_call_keeps_the_text_the_model_wrote() {
        // Nothing ran, so nothing is taken away: stripping the syntax off a
        // call that will never run would leave no record of it anywhere.
        let written = format!("Reading it now.\n{FRAMED_READ}");
        let (content, _) = recover(
            vec![ContentBlock::Text {
                text: written.clone(),
            }],
            ResponseStatus::OutputTruncated,
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
        let (content, events) = recover(
            vec![ContentBlock::Text {
                text: format!("Reading it now.\n{FRAMED_READ}"),
            }],
            ResponseStatus::EndTurn,
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }, ContentBlock::ToolUse { name, .. }]
                if text == "Reading it now.\n" && name == "read_file"
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name, call_id }]
                if tool_name == "read_file" && call_id == "repaired_0"
        ));
    }

    #[test]
    fn a_reply_without_frames_is_preserved_exactly() {
        let answer = "  Just an answer.\n";
        let (content, events) = recover(
            vec![ContentBlock::Text {
                text: answer.into(),
            }],
            ResponseStatus::EndTurn,
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }] if text == answer
        ));
        assert!(events.is_empty());
    }
}

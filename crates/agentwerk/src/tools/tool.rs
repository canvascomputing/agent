//! Defines callable agent tools and executes their handlers.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::agents::tasks::{Run, Werk};
pub(crate) use crate::event::Event;
use crate::prompts::directives::{
    DirectiveStore, ARGUMENTS_REJECTED, TOOL_OUTPUT_EMPTY, TOOL_OUTPUT_OFFLOADED, TOOL_TIMED_OUT,
};
use crate::prompts::Text;
use crate::providers::ContentBlock;
use crate::schemas::Schema;

/// The largest result one tool may return. Anything longer is written to
/// `<task-dir>/outputs/<tool_use_id>.txt` and replaced with a short stub.
/// Roughly 12 500 tokens.
const PER_TOOL_CAP: usize = 50_000;

/// The largest total one turn's tool results may reach. When several tools each
/// return an acceptable size, the largest are written out until the turn fits.
/// Roughly 50 000 tokens.
const PER_TURN_CAP: usize = 200_000;

/// How much of the original output the stub still shows. It ends at the last
/// newline in that window, or at a character boundary, so a multi-byte
/// character is never cut in half.
const PREVIEW_CHARS: usize = 2_000;

#[derive(Clone)]
pub(crate) struct ToolContext {
    pub(crate) dir: PathBuf,
    pub(crate) run: Option<Arc<Run>>,
    pub(crate) werk: Option<Arc<Werk>>,
    pub(crate) agent_id: Option<String>,
    pub(crate) task_id: Option<String>,
    pub(crate) directives: Arc<DirectiveStore>,
}

impl ToolContext {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            run: None,
            werk: None,
            agent_id: None,
            task_id: None,
            directives: Arc::new(DirectiveStore::default()),
        }
    }

    pub(crate) fn run(mut self, run: Arc<Run>) -> Self {
        self.run = Some(run);
        self
    }

    pub(crate) fn werk(mut self, werk: Arc<Werk>) -> Self {
        self.werk = Some(werk);
        self
    }

    pub(crate) fn agent_id(mut self, name: String) -> Self {
        self.agent_id = Some(name);
        self
    }

    pub(crate) fn task_id(mut self, id: String) -> Self {
        self.task_id = Some(id);
        self
    }

    pub(crate) fn directives(mut self, directives: Arc<DirectiveStore>) -> Self {
        self.directives = directives;
        self
    }

    /// Publish `event` for the task and agent this call runs for. A context
    /// with no Werk publishes nothing; the call still runs.
    pub(crate) fn emit_event(&self, event: Event) {
        let Some(werk) = &self.werk else {
            return;
        };
        let id = self.task_id.as_deref().unwrap_or_default();
        let agent = self.agent_id.as_deref().unwrap_or_default();
        werk.emit_event(event.task_id(id).agent_id(agent));
    }

    pub(crate) async fn cancelled(&self) {
        match &self.run {
            Some(run) => run.until_draining().await,
            None => std::future::pending::<()>().await,
        }
    }
}

impl Event {
    #[doc(hidden)]
    pub(crate) fn success(content: impl Into<String>) -> Self {
        Event::tool_call_finished(content)
    }

    #[doc(hidden)]
    pub(crate) fn error(content: impl Into<String>) -> Self {
        Event::tool_call_failed(content)
    }

    pub(crate) fn tool_timed_out(
        tool: &str,
        timeout: Duration,
        directives: &DirectiveStore,
    ) -> Self {
        Event::error(directives.render(
            TOOL_TIMED_OUT,
            &[
                ("tool", tool),
                ("milliseconds", &timeout.as_millis().to_string()),
            ],
        ))
        .directive(TOOL_TIMED_OUT)
    }

    /// The text returned to the model by a terminal tool-call event.
    pub(crate) fn get_content(&self) -> &str {
        let key = if self.name == Event::TOOL_CALL_FINISHED {
            "output"
        } else {
            "message"
        };
        self.data
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub(crate) fn tool_failure(content: impl Into<String>, reason: &'static str) -> Self {
        Event::new(Event::TOOL_CALL_FAILED).data(serde_json::json!({
            "kind": reason,
            "message": content.into(),
        }))
    }

    fn content_mut(&mut self) -> Option<&mut String> {
        let key = if self.name == Event::TOOL_CALL_FINISHED {
            "output"
        } else {
            "message"
        };
        match self.data.get_mut(key)? {
            Value::String(content) => Some(content),
            _ => None,
        }
    }

    pub(crate) fn repairs(&self) -> impl Iterator<Item = &str> {
        self.data
            .get("repairs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
    }

    pub(crate) fn prepend_repairs(&mut self, repairs: impl IntoIterator<Item = String>) {
        let mut combined: Vec<Value> = repairs.into_iter().map(Value::String).collect();
        combined.extend(self.repairs().map(|repair| repair.to_string().into()));
        if !combined.is_empty() {
            self.data
                .as_object_mut()
                .expect("validated tool event data")
                .insert("repairs".into(), combined.into());
        }
    }

    pub(crate) fn output_path(&self) -> Option<&str> {
        self.data.get("output_path").and_then(Value::as_str)
    }

    fn set_output_path(&mut self, path: &std::path::Path) {
        self.data
            .as_object_mut()
            .expect("validated tool event data")
            .insert(
                "output_path".into(),
                path.to_string_lossy().into_owned().into(),
            );
    }

    pub(crate) fn cap_tool_results(
        calls: &[ContentBlock],
        results: &mut [Event],
        ctx: &ToolContext,
    ) {
        cap_results(calls, results, ctx);
    }
}

type ToolHandler = Arc<
    dyn Fn(Value, &ToolContext) -> Pin<Box<dyn Future<Output = Event> + Send + '_>> + Send + Sync,
>;

/// How a tool determines the limit for one invocation.
#[derive(Clone)]
enum TimeoutPolicy {
    Unlimited,
    Fixed(Duration),
    Input {
        field: &'static str,
        default: Duration,
        maximum: Duration,
    },
}

impl TimeoutPolicy {
    fn resolve(&self, input: &Value) -> Option<Duration> {
        let duration = match self {
            TimeoutPolicy::Unlimited => return None,
            TimeoutPolicy::Fixed(timeout) => return Some(*timeout),
            TimeoutPolicy::Input {
                field,
                default,
                maximum,
            } => input
                .get(*field)
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(*default)
                .min(*maximum),
        };
        (!duration.is_zero()).then_some(duration)
    }
}

/// A tool an agent may call: a name, a description, a JSON Schema for the
/// arguments, and the handler that runs.
///
/// The handler names its own argument type, so a typed tool deserializes
/// nowhere in its body; `Value` takes the JSON as it arrived. State the
/// handler needs is captured in its closure. Copying a `Tool` is cheap and
/// shares the handler.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::{Event, tools::Tool};
/// use serde_json::{json, Value};
///
/// let greet = Tool::new("greet")
///     .description("Say hello to a name.")
///     .schema(json!({
///         "type": "object",
///         "properties": { "name": { "type": "string" } },
///         "required": ["name"]
///     }))
///     .concurrent(true)
///     .handler(|input: Value| async move {
///         let name = input["name"].as_str().unwrap_or("world");
///         Event::tool_call_finished(format!("Hello, {name}!"))
///     });
///
/// Agent::new().tool(greet);
/// ```
///
/// An incomplete tool is rejected when it is registered:
///
/// ```should_panic
/// use agentwerk::Agent;
/// use agentwerk::tools::Tool;
///
/// let _agent = Agent::new().tool(Tool::new("greet"));
/// ```
///
/// Path declarations are not part of a tool definition:
///
/// ```compile_fail
/// use agentwerk::tools::Tool;
///
/// let _tool = Tool::new("read").paths(["path"]);
/// ```
#[derive(Clone)]
pub struct Tool {
    name: String,
    description: Option<String>,
    schema: Schema,
    concurrent: bool,
    timeout: TimeoutPolicy,
    handler: Option<ToolHandler>,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("concurrent", &self.concurrent)
            .finish()
    }
}

impl Tool {
    pub(crate) fn find_tool<'a>(tools: &'a [Self], tool_name: &str) -> Option<&'a Self> {
        let tool_name = tool_name.trim();
        if let Some(found) = tools.iter().find(|tool| tool.get_name() == tool_name) {
            return Some(found);
        }

        let tool_name = Self::normalize_name(tool_name);
        let mut folded = tools
            .iter()
            .filter(|tool| Self::normalize_name(tool.get_name()) == tool_name);
        let found = folded.next()?;
        folded.next().is_none().then_some(found)
    }

    fn normalize_name(tool_name: &str) -> String {
        let name = tool_name.trim().to_lowercase().replace('-', "_");
        match name.strip_suffix("_tool") {
            Some(stem) if !stem.is_empty() => stem.to_string(),
            _ => name,
        }
    }

    /// Create the tool the model calls by `name`. Say what it does with
    /// `.description(...)`, what it accepts with `.schema(...)`, and what it
    /// runs with `.handler(...)` before registering it on an agent.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            schema: Schema::new(serde_json::json!({"type": "object", "properties": {}}))
                .expect("a literal object schema compiles"),
            concurrent: false,
            timeout: TimeoutPolicy::Unlimited,
            handler: None,
        }
    }

    /// Say what the tool does, in the words the model reads.
    ///
    /// A string is the description itself; a `&Path` or `PathBuf` names the
    /// file holding it, which panics when that file cannot be read.
    pub fn description(mut self, description: impl Into<Text>) -> Self {
        self.description = Some(description.into().into_string());
        self
    }

    /// Define what the tool accepts, as a JSON Schema document or the text of
    /// one. Panics on a document `Schema` refuses, naming this tool: an
    /// uncheckable tool is a mistake here, not one the agent should discover at
    /// call time.
    pub fn schema<S>(mut self, schema: S) -> Self
    where
        S: TryInto<Schema>,
        S::Error: std::fmt::Display,
    {
        self.schema = schema.try_into().unwrap_or_else(|error| {
            panic!(
                "tool `{}` declares a schema that does not compile: {error}",
                self.name
            )
        });
        self
    }

    /// Run this tool in parallel with the turn's other concurrent calls. Set it
    /// for a tool with no side effects.
    pub fn concurrent(mut self, concurrent: bool) -> Self {
        self.concurrent = concurrent;
        self
    }

    /// Limit one invocation of this tool. [`Duration::ZERO`] means no limit.
    ///
    /// The limit starts after the arguments have been validated and the call
    /// has been admitted for execution. Calling this replaces a built-in
    /// tool's default timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = if timeout.is_zero() {
            TimeoutPolicy::Unlimited
        } else {
            TimeoutPolicy::Fixed(timeout)
        };
        self
    }

    pub(crate) fn default_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = TimeoutPolicy::Fixed(timeout);
        self
    }

    pub(crate) fn timeout_from_input(
        mut self,
        field: &'static str,
        default: Duration,
        maximum: Duration,
    ) -> Self {
        self.timeout = TimeoutPolicy::Input {
            field,
            default,
            maximum,
        };
        self
    }

    /// Define what runs when the model calls this tool. A bare `async` block
    /// works, and the handler names the type its arguments are read into.
    pub fn handler<A, F, Fut>(mut self, handler: F) -> Self
    where
        A: serde::de::DeserializeOwned + Send + 'static,
        F: Fn(A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Event> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.handler = Some(read_arguments_then(
            self.name.clone(),
            move |args, ctx: ToolContext| {
                let handler = Arc::clone(&handler);
                async move {
                    tokio::select! {
                        biased;
                        _ = ctx.cancelled() => Event::error("tool call cancelled"),
                        result = handler(args) => result,
                    }
                }
            },
        ));
        self
    }

    pub(crate) fn handler_with_context<A, F, Fut>(mut self, handler: F) -> Self
    where
        A: serde::de::DeserializeOwned + Send + 'static,
        F: Fn(A, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Event> + Send + 'static,
    {
        self.handler = Some(read_arguments_then(self.name.clone(), handler));
        self
    }

    pub(crate) fn require_description_and_handler(&self) {
        assert!(
            self.description.is_some(),
            "description required for tool `{}`: call Tool::description(..)",
            self.name
        );
        assert!(
            self.handler.is_some(),
            "handler required for tool `{}`: call Tool::handler(..)",
            self.name
        );
    }

    /// Run this tool's handler on arguments already checked by the agent.
    pub(crate) async fn call(&self, input: Value, ctx: &ToolContext) -> Event {
        (self.handler.as_ref().unwrap_or_else(|| {
            panic!(
                "handler required for tool `{}`: call Tool::handler(..)",
                self.name
            )
        }))(input, ctx)
        .await
    }

    /// Validate one call against this tool's schema, run its handler, and
    /// validate the terminal event it returns.
    pub(crate) async fn invoke(&self, input: Value, ctx: &ToolContext) -> Event {
        let (input, repairs) = match self.schema.validate(input) {
            Ok(validated) => validated,
            Err(violations) => {
                return Event::tool_failure(
                    crate::prompts::arguments_retry_detail(
                        self.get_name(),
                        &violations.to_string(),
                        Some(self.schema.get_raw_schema()),
                        &ctx.directives,
                    ),
                    "schema_failed",
                )
                .directive(ARGUMENTS_REJECTED);
            }
        };
        let timeout = self.timeout.resolve(&input);
        let call = self.call(input, ctx);
        let event = match timeout {
            Some(duration) => match tokio::time::timeout(duration, call).await {
                Ok(event) => event,
                Err(_) => Event::tool_timed_out(self.get_name(), duration, &ctx.directives),
            },
            None => call.await,
        };
        let mut result = validate_tool_event(self.get_name(), event);
        result.prepend_repairs(repairs.iter().map(|pointer| retype_message(pointer)));
        result
    }

    /// The name the model calls the tool by.
    pub fn get_name(&self) -> &str {
        &self.name
    }

    /// What the tool does, in the words the model reads.
    pub fn get_description(&self) -> &str {
        self.description.as_deref().unwrap_or_else(|| {
            panic!(
                "description required for tool `{}`: call Tool::description(..)",
                self.name
            )
        })
    }

    /// The arguments this tool accepts, compiled.
    pub fn get_input_schema(&self) -> &Schema {
        &self.schema
    }

    /// Whether the agent may run this tool alongside the turn's other
    /// concurrent calls.
    pub(crate) fn is_concurrent(&self) -> bool {
        self.concurrent
    }
}

/// Fold a typed handler into the stored form: deserialize the checked
/// arguments, then run the handler on what they read as.
fn read_arguments_then<A, F, Fut>(name: String, handler: F) -> ToolHandler
where
    A: serde::de::DeserializeOwned + Send + 'static,
    F: Fn(A, ToolContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Event> + Send + 'static,
{
    Arc::new(move |input, ctx| match serde_json::from_value::<A>(input) {
        Ok(args) => Box::pin(handler(args, ctx.clone())),
        // The schema accepted this call, so the tool's schema and its
        // argument type disagree: the author's mistake, not the model's.
        // Reporting it as a schema failure would show the model a document
        // its call already satisfied.
        Err(error) => Box::pin(std::future::ready(Event::error(format!(
            "`{name}` could not read its arguments: {error}"
        )))),
    })
}

fn validate_tool_event(tool: &str, mut event: Event) -> Event {
    let repairs_are_valid = event.data.get("repairs").is_none_or(|repairs| {
        repairs
            .as_array()
            .is_some_and(|repairs| repairs.iter().all(Value::is_string))
    });
    let valid = match event.name.as_str() {
        Event::TOOL_CALL_FINISHED => {
            event.data.get("output").is_some_and(Value::is_string)
                && event.data.get("output_path").is_none_or(Value::is_string)
                && repairs_are_valid
        }
        Event::TOOL_CALL_FAILED => {
            event.data.get("message").is_some_and(Value::is_string)
                && event.data.get("reason").is_none()
                && repairs_are_valid
        }
        _ => false,
    };
    if !valid {
        return Event::error(format!(
            "tool `{tool}` returned an invalid event; expected tool_call_finished with string output or tool_call_failed with string message"
        ));
    }
    if event.name == Event::TOOL_CALL_FAILED {
        let data = event.data.as_object_mut().expect("validated object data");
        let reason = data
            .entry("kind")
            .or_insert_with(|| "execution_failed".into());
        if !matches!(
            reason.as_str(),
            Some("not_found" | "execution_failed" | "schema_failed")
        ) {
            return Event::error(format!("tool `{tool}` returned an invalid failure kind"));
        }
    } else {
        // Only successful persistence by the runtime may name an offloaded
        // output; a handler cannot claim it wrote a file the loop should keep.
        event
            .data
            .as_object_mut()
            .expect("validated object data")
            .remove("output_path");
        if event
            .data
            .get("repairs")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            event
                .data
                .as_object_mut()
                .expect("validated object data")
                .remove("repairs");
        }
    }
    event
}

/// Name a retype by the JSON pointer it happened at; the empty pointer is the
/// value as a whole. One verb covers both rewrites: decoding a payload the
/// model wrote as text is the whole-value case of retyping it.
pub(crate) fn retype_message(pointer: &str) -> String {
    match pointer {
        "" => "retyped".to_string(),
        path => format!("{path} retyped"),
    }
}

/// Stand in for every empty and oversized result, so one turn's reply always
/// fits, recording on each capped result where its original went.
///
/// `results` answer `calls`, in the same order. Only a
/// `tool_call_finished` output is ever rewritten: a failure's message is what the
/// model must read to recover, and is short by construction.
fn cap_results(calls: &[ContentBlock], results: &mut [Event], ctx: &ToolContext) {
    for (call, result) in calls.iter().zip(results.iter_mut()) {
        let ContentBlock::ToolUse { id, name, .. } = call else {
            continue;
        };
        replace_empty_output(result, name, &ctx.directives);
        cap_oversized_result(result, ctx, id, PER_TOOL_CAP);
    }
    cap_aggregate_outputs(calls, results, ctx, PER_TURN_CAP);
}

/// Put a placeholder in place of an empty result, since empty content has upset
/// LLM providers.
fn replace_empty_output(result: &mut Event, tool_name: &str, directives: &DirectiveStore) {
    if result.name != Event::TOOL_CALL_FINISHED {
        return;
    }
    let Some(content) = result.content_mut() else {
        return;
    };
    if content.is_empty() {
        *content = directives.render(TOOL_OUTPUT_EMPTY, &[("tool", tool_name)]);
    }
}

/// Replace an oversized result with a stub, writing the original under the
/// task's outputs directory.
///
/// A failure passes through, being short by construction, and so does the raw
/// content when the write fails.
fn cap_oversized_result(result: &mut Event, ctx: &ToolContext, call_id: &str, per_tool_cap: usize) {
    if result.name != Event::TOOL_CALL_FINISHED {
        return;
    }
    let Some(content) = result.content_mut() else {
        return;
    };
    if content.len() <= per_tool_cap {
        return;
    }
    if let Some(path) = write_out(content, ctx, call_id) {
        result.set_output_path(&path);
    }
}

/// While one turn's results are too large together, write out the largest
/// success that is not already a stub. It stops once the turn fits, or once
/// nothing left can be written out.
fn cap_aggregate_outputs(
    calls: &[ContentBlock],
    results: &mut [Event],
    ctx: &ToolContext,
    per_turn_cap: usize,
) {
    loop {
        let total: usize = results
            .iter()
            .map(|result| result.get_content().len())
            .sum();
        if total <= per_turn_cap {
            return;
        }
        let Some((call_id, result)) = largest_inline_success(calls, results) else {
            return;
        };
        let Some(content) = result.content_mut() else {
            return;
        };
        let Some(path) = write_out(content, ctx, call_id) else {
            // Persistence failed; nothing further this pass can do.
            return;
        };
        result.set_output_path(&path);
    }
}

/// The largest success still inline, which is the next one to write out.
fn largest_inline_success<'a>(
    calls: &'a [ContentBlock],
    results: &'a mut [Event],
) -> Option<(&'a str, &'a mut Event)> {
    calls
        .iter()
        .zip(results.iter_mut())
        .filter_map(|(call, result)| {
            let ContentBlock::ToolUse { id, .. } = call else {
                return None;
            };
            (result.name == Event::TOOL_CALL_FINISHED
                && !result.get_content().starts_with(OVERSIZED_STUB_TAG_OPEN))
            .then_some((id.as_str(), result))
        })
        .max_by_key(|(_, result)| result.get_content().len())
}

/// Write `content` out under the task's outputs directory and leave the stub
/// the model reads in its place, giving back where the original went.
///
/// `None` when the write fails, and then `content` is left as it was.
fn write_out(content: &mut String, ctx: &ToolContext, call_id: &str) -> Option<PathBuf> {
    let output = persist_output(ctx, call_id, content)?;
    let preview = truncate_preview(content);
    let stub =
        format_oversized_tool_result(content.len(), &output.display, preview, &ctx.directives);
    *content = stub;
    Some(output.rel)
}

/// Write `content` under the task's outputs directory, reporting both the
/// path relative to the session and the path on disk.
///
/// `None` when the context names no task, no Werk is attached, or
/// the write fails. Like the rest of the logging, it is best effort.
fn persist_output(ctx: &ToolContext, tool_use_id: &str, content: &str) -> Option<PersistedOutput> {
    let werk = ctx.werk.as_ref()?;
    let id = ctx.task_id.as_deref()?;
    let rel = werk.write_tool_output(id, tool_use_id, content)?;
    let display = werk.get_dir().join(&rel);
    Some(PersistedOutput { rel, display })
}

struct PersistedOutput {
    rel: PathBuf,
    display: PathBuf,
}

const OVERSIZED_STUB_TAG_OPEN: &str = "<persisted-output>";
const OVERSIZED_STUB_TAG_CLOSE: &str = "</persisted-output>";

/// Build the stub the model sees in place of an oversized result: how large it
/// was, where it went, and how it starts.
fn format_oversized_tool_result(
    original_len: usize,
    path: &Path,
    preview: &str,
    directives: &DirectiveStore,
) -> String {
    // The tags stay out of the directive: `cap_aggregate_outputs` reads the
    // opening one to tell an already-stubbed result from a fresh one.
    let body = directives.render(
        TOOL_OUTPUT_OFFLOADED,
        &[
            ("size", &format_bytes(original_len)),
            ("path", &path.display().to_string()),
            ("preview_size", &format_bytes(preview.len())),
            ("preview", preview),
        ],
    );
    format!("{OVERSIZED_STUB_TAG_OPEN}{body}\n{OVERSIZED_STUB_TAG_CLOSE}")
}

/// The first `PREVIEW_CHARS` bytes of `content`, ending at the last newline in
/// that window or at a character boundary.
///
/// Move the window to a character boundary because `PREVIEW_CHARS` may split a multibyte character.
fn truncate_preview(content: &str) -> &str {
    let window = utf8_boundary_floor(content, PREVIEW_CHARS.min(content.len()));
    let cut = content[..window]
        .rfind('\n')
        .map(|newline| newline + 1)
        .unwrap_or(window);
    &content[..cut]
}

fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if (bytes as f64) < MB {
        format!("{:.1} KB", bytes as f64 / KB)
    } else {
        format!("{:.1} MB", bytes as f64 / MB)
    }
}

/// Move an index back to the nearest character boundary, at most three bytes.
fn utf8_boundary_floor(content: &str, mut index: usize) -> usize {
    while index > 0 && !content.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent::Agent;

    /// Every tool agentwerk registers, for the checks that hold across all of
    /// them. The knowledge store is temporary; its tool is read, not used.
    fn built_in_tools() -> (Vec<Tool>, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = crate::agents::knowledge::Knowledge::load(dir.path()).unwrap();
        let tools: Vec<Tool> = vec![
            crate::tools::ReadFileTool.into(),
            crate::tools::WriteFileTool.into(),
            crate::tools::EditFileTool.into(),
            crate::tools::GlobTool.into(),
            crate::tools::GrepTool.into(),
            crate::tools::ListDirectoryTool.into(),
            crate::tools::FetchTool::new().into(),
            crate::tools::KnowledgeTool::new(store).into(),
            crate::tools::CommandTool::new("git").allow("git *").into(),
            crate::tools::EventTool.into(),
            crate::tools::FinishTool.into(),
            crate::tools::TaskTool.into(),
        ];
        (tools, dir)
    }

    #[tokio::test]
    async fn a_tool_over_its_configured_timeout_fails() {
        let tool = Tool::new("slow")
            .description("slow")
            .timeout(Duration::from_millis(10))
            .handler(|_: Value| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Event::success("late")
            });

        let result = tool.invoke(serde_json::json!({}), &test_ctx()).await;

        assert_eq!(result.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(result.get_directive(), Some(TOOL_TIMED_OUT));
        assert_eq!(result.get_data()["kind"], "execution_failed");
        assert!(result.get_content().contains("slow"));
        assert!(result.get_content().contains("10ms"));
    }

    #[tokio::test]
    async fn a_fetch_timeout_uses_the_shared_tool_event() {
        let tool = Tool::from(crate::tools::FetchTool::new())
            .timeout(Duration::from_millis(10))
            .handler(|_: Value| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Event::success("late")
            });

        let result = tool
            .invoke(
                serde_json::json!({"url": "https://example.com"}),
                &test_ctx(),
            )
            .await;

        assert_eq!(result.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(result.get_directive(), Some(TOOL_TIMED_OUT));
        assert!(result
            .get_content()
            .contains("Tool `fetch` timed out after 10ms"));
    }

    #[tokio::test]
    async fn a_zero_tool_timeout_is_infinite() {
        let tool = Tool::new("patient")
            .description("patient")
            .timeout(Duration::ZERO)
            .handler(|_: Value| async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Event::success("done")
            });

        let result = tool.invoke(serde_json::json!({}), &test_ctx()).await;

        assert_eq!(result.get_name(), Event::TOOL_CALL_FINISHED);
        assert_eq!(result.get_content(), "done");
    }

    #[tokio::test]
    async fn argument_validation_happens_before_the_tool_deadline_starts() {
        let tool = Tool::new("strict")
            .description("strict")
            .schema(serde_json::json!({
                "type": "object",
                "properties": {"required": {"type": "string"}},
                "required": ["required"],
            }))
            .timeout(Duration::from_nanos(1))
            .handler(|_: Value| async { Event::success("unreachable") });

        let result = tool.invoke(serde_json::json!({}), &test_ctx()).await;

        assert_eq!(result.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(result.get_directive(), Some(ARGUMENTS_REJECTED));
    }

    #[test]
    fn built_in_timeout_defaults_and_command_input_are_preserved() {
        let empty = serde_json::json!({});
        assert_eq!(Tool::new("custom").timeout.resolve(&empty), None);
        assert_eq!(
            Tool::from(crate::tools::FetchTool::new())
                .timeout
                .resolve(&empty),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            Tool::from(crate::tools::GrepTool).timeout.resolve(&empty),
            Some(Duration::from_secs(180))
        );
        assert_eq!(
            Tool::from(crate::tools::GrepTool)
                .timeout(Duration::from_secs(5))
                .timeout
                .resolve(&empty),
            Some(Duration::from_secs(5))
        );

        let command = Tool::from(crate::tools::CommandTool::new("echo"));
        assert_eq!(
            command.timeout.resolve(&empty),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            command
                .timeout
                .resolve(&serde_json::json!({"timeout_ms": 900_000})),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            command
                .timeout
                .resolve(&serde_json::json!({"timeout_ms": 0})),
            None
        );
    }

    #[test]
    fn fetch_timeout_overrides_its_default_before_conversion() {
        let input = serde_json::json!({});
        let before_impersonation = Tool::from(
            crate::tools::FetchTool::new()
                .timeout(Duration::from_secs(15))
                .impersonate(),
        );
        let after_impersonation = Tool::from(
            crate::tools::FetchTool::new()
                .impersonate()
                .timeout(Duration::from_secs(30)),
        );
        let unlimited = Tool::from(crate::tools::FetchTool::new().timeout(Duration::ZERO));

        assert_eq!(
            before_impersonation.timeout.resolve(&input),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            after_impersonation.timeout.resolve(&input),
            Some(Duration::from_secs(30))
        );
        assert_eq!(unlimited.timeout.resolve(&input), None);
    }

    /// Each built-in states its name and concurrency as a literal in its `From`
    /// conversion and reads its description from the file beside it, so a typo
    /// in either literal, or a definition that stopped being included, is what
    /// this pins.
    #[test]
    fn every_built_in_tool_declares_what_the_model_is_shown() {
        let (tools, _dir) = built_in_tools();
        let declared: Vec<(&str, bool)> = tools
            .iter()
            .map(|tool| (tool.get_name(), tool.is_concurrent()))
            .collect();
        assert_eq!(
            declared,
            [
                ("read_file", true),
                ("write_file", false),
                ("edit_file", false),
                ("glob", true),
                ("grep", true),
                ("list_directory", true),
                ("fetch", true),
                ("knowledge", false),
                ("git", false),
                ("event", false),
                ("finish", false),
                ("task", false),
            ]
        );
        for tool in &tools {
            assert!(
                !tool.get_description().is_empty(),
                "empty description for {}",
                tool.get_name(),
            );
            // Execution relies on this, so a tool that declares
            // something else loses the check that its arguments are an object.
            assert_eq!(
                tool.get_input_schema().get_raw_schema()["type"],
                "object",
                "arguments are not an object for {}",
                tool.get_name(),
            );
        }
    }

    #[test]
    fn every_example_a_built_in_tool_shows_is_a_call_its_own_schema_accepts() {
        // An example needing a repair, or failing outright, teaches the model a
        // shape it will be corrected for.
        let (tools, _dir) = built_in_tools();
        for tool in &tools {
            let schema = tool.get_input_schema();
            let examples = schema.get_raw_schema()["examples"]
                .as_array()
                .unwrap_or_else(|| panic!("{} shows no examples", tool.get_name()))
                .clone();
            for example in examples {
                let (_, repaired) = schema
                    .validate(example.clone())
                    .unwrap_or_else(|violations| panic!("{}: {violations}", tool.get_name()));
                assert!(
                    repaired.is_empty(),
                    "{} repaired {example}",
                    tool.get_name()
                );
            }
        }
    }

    #[test]
    fn registering_a_name_twice_leaves_the_later_tool() {
        let agent = Agent::new()
            .tool(mock_tool("echo", true, "first"))
            .tool(mock_tool("echo", true, "second"));
        let echoes: Vec<_> = agent
            .tool_list()
            .iter()
            .filter(|tool| tool.get_name() == "echo")
            .collect();
        assert_eq!(echoes.len(), 1);
    }

    #[test]
    #[should_panic(expected = "description required for tool `incomplete`")]
    fn registering_a_tool_without_a_description_panics() {
        Agent::new().tool(Tool::new("incomplete").handler(|_: Value| async { Event::success("") }));
    }

    #[test]
    #[should_panic(expected = "handler required for tool `incomplete`")]
    fn registering_a_tool_without_a_handler_panics() {
        Agent::new().tool(Tool::new("incomplete").description("incomplete"));
    }

    #[tokio::test]
    async fn repeated_configuration_keeps_the_last_value() {
        let tool = Tool::new("configured")
            .description("first")
            .description("second")
            .handler(|_: Value| async { Event::success("first") })
            .handler(|_: Value| async { Event::success("second") });

        assert_eq!(tool.get_description(), "second");
        assert_eq!(
            tool.call(serde_json::json!({}), &test_ctx())
                .await
                .get_content(),
            "second"
        );
    }

    /// The mock the registry tests share.
    fn mock_tool(name: &str, concurrent: bool, result: &str) -> Tool {
        let result = result.to_string();
        Tool::new(name)
            .description("mock")
            .schema(serde_json::json!({"type": "object"}))
            .concurrent(concurrent)
            .handler(move |_: Value| {
                let result = result.clone();
                async move { Event::success(result) }
            })
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn registration_and_lookup() {
        let agent = Agent::new().tool(mock_tool("read_file", true, "file contents"));
        assert!(agent.get_tool(agent.tool_list(), "read_file").is_some());
        assert!(agent.get_tool(agent.tool_list(), "nonexistent").is_none());
    }

    #[test]
    fn contains_answers_on_the_exact_name_only() {
        let agent = Agent::new().tool(mock_tool("grep", true, "matches"));
        let exact = |name: &str| agent.tool_list().iter().any(|tool| tool.get_name() == name);
        assert!(exact("grep"));
        assert!(!exact("glob"));
        assert!(
            !exact("grep_tool"),
            "a folded spelling is not the registered name",
        );
    }

    #[test]
    fn resolves_a_name_carrying_a_tool_suffix() {
        let agent = Agent::new().tool(mock_tool("grep", true, "matches"));
        let tool = agent
            .get_tool(agent.tool_list(), "grep_tool")
            .expect("suffix should fold away");
        assert_eq!(tool.get_name(), "grep");
    }

    #[test]
    fn resolves_a_name_the_model_hyphenated() {
        let agent = Agent::new().tool(mock_tool("read_file", true, "file contents"));
        let tool = agent
            .get_tool(agent.tool_list(), "Read-File")
            .expect("case and hyphen should fold");
        assert_eq!(tool.get_name(), "read_file");
    }

    #[test]
    fn refuses_a_name_two_tools_share_a_key() {
        let agent = Agent::new()
            .tool(mock_tool("grep", true, "builtin"))
            .tool(mock_tool("grep_tool", true, "host tool"));
        assert!(agent.get_tool(agent.tool_list(), "Grep").is_none());
        // Each registered name still reaches its own tool: exact match wins.
        assert_eq!(
            agent
                .get_tool(agent.tool_list(), "grep")
                .unwrap()
                .get_name(),
            "grep"
        );
        assert_eq!(
            agent
                .get_tool(agent.tool_list(), "grep_tool")
                .unwrap()
                .get_name(),
            "grep_tool"
        );
    }

    #[test]
    fn a_description_read_from_a_file_keeps_its_prose_and_loses_the_closing_newline() {
        let tool = Tool::new("demo")
            .description("Do the demo thing.\n\n- Returns nothing useful.\n")
            .handler(|_: Value| async { Event::success("") });
        assert_eq!(
            tool.get_description(),
            "Do the demo thing.\n\n- Returns nothing useful."
        );
    }

    #[test]
    fn a_description_named_by_a_path_is_read_from_that_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let file = dir.path().join("demo.tool.md");
        std::fs::write(&file, "Do the demo thing.\n").unwrap();
        let tool = Tool::new("demo")
            .description(file.as_path())
            .handler(|_: Value| async { Event::success("") });
        assert_eq!(tool.get_description(), "Do the demo thing.");
    }

    #[test]
    fn a_schema_written_as_json_text_is_the_one_the_tool_advertises() {
        let tool = Tool::new("demo")
            .description("Do the demo thing.")
            .schema(
                r#"{"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]}"#,
            )
            .handler(|_: Value| async { Event::success("") });
        let document = tool.get_input_schema().get_raw_schema();
        assert_eq!(document["properties"]["x"]["type"], "string");
        assert_eq!(document["required"][0], "x");
    }

    #[test]
    fn registration_keeps_every_distinct_tool() {
        let agent = Agent::new()
            .tool(mock_tool("read", true, "ok"))
            .tool(mock_tool("write", false, "ok"));
        assert!(agent.get_tool(agent.tool_list(), "read").is_some());
        assert!(agent.get_tool(agent.tool_list(), "write").is_some());
    }

    #[tokio::test]
    async fn a_rejected_call_reads_back_the_one_schema_its_tool_holds() {
        let task = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"partial_sum": {"type": "integer"}},
            "required": ["partial_sum"],
        }))
        .unwrap();
        let finish = crate::tools::FinishTool::from_schema(Some(task), None);
        let shown = finish.get_input_schema().get_raw_schema().clone();
        let content = finish
            .invoke(serde_json::json!({}), &test_ctx())
            .await
            .get_content()
            .to_string();

        assert!(shown["properties"]["partial_sum"].is_object(), "{shown}");
        assert_eq!(shown["required"], serde_json::json!(["partial_sum"]));
        assert!(
            content.contains(&serde_json::to_string_pretty(&shown).unwrap()),
            "{content}"
        );
    }

    #[test]
    fn a_tool_list_clone_keeps_its_tools() {
        let tools = vec![mock_tool("t", true, "ok")];
        let cloned = tools.clone();
        assert_eq!(cloned.len(), 1);
    }

    #[tokio::test]
    async fn non_object_input_is_reported_as_such_and_never_reaches_the_tool() {
        let result = mock_tool("grep", true, "matches")
            .invoke(Value::String(r#"{"pattern": "exec""#.into()), &test_ctx())
            .await;
        assert_eq!(result.get_data()["kind"], "schema_failed");
        let content = result.get_content();
        assert!(
            content.contains("expected type object, got string"),
            "{content}"
        );
        assert!(
            content.contains("send it as JSON, not as a string"),
            "{content}"
        );
        assert_ne!(content, "matches");
    }

    /// A tool that declares what its one argument must be, so dispatch has
    /// something to check a call against. It answers with the argument it
    /// received, which is what the retype tests read.
    fn typed_tool() -> Tool {
        Tool::new("typed")
            .description("typed")
            .schema(serde_json::json!({
                "type": "object",
                "properties": { "count": { "type": "integer" } },
                "required": ["count"],
            }))
            .handler(|input: Value| async move { Event::success(input["count"].to_string()) })
    }

    async fn call_typed(input: Value) -> (String, bool) {
        call_typed_in(input, &test_ctx()).await
    }

    async fn call_typed_in(input: Value, ctx: &ToolContext) -> (String, bool) {
        let result = typed_tool().invoke(input, ctx).await;
        let succeeded = result.get_name() == Event::TOOL_CALL_FINISHED;
        (result.get_content().to_string(), succeeded)
    }

    #[tokio::test]
    async fn arguments_matching_the_declared_schema_reach_the_tool() {
        let (content, succeeded) = call_typed(serde_json::json!({"count": 3})).await;
        assert!(succeeded);
        assert_eq!(content, "3");
    }

    #[tokio::test]
    async fn an_argument_the_model_quoted_is_retyped_before_the_tool_runs() {
        let (content, succeeded) = call_typed(serde_json::json!({"count": "3"})).await;
        assert!(succeeded);
        assert_eq!(content, "3");
    }

    #[tokio::test]
    async fn arguments_the_model_quoted_whole_are_decoded_before_the_tool_runs() {
        let (content, succeeded) = call_typed(Value::String(r#"{"count": 3}"#.into())).await;
        assert!(succeeded);
        assert_eq!(content, "3");
    }

    #[tokio::test]
    async fn an_argument_no_retype_recovers_is_named_and_never_reaches_the_tool() {
        let (content, succeeded) = call_typed(serde_json::json!({"count": "three"})).await;
        assert!(!succeeded);
        assert!(content.contains("count"), "{content}");
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_named_and_never_reaches_the_tool() {
        let (content, succeeded) = call_typed(serde_json::json!({})).await;
        assert!(!succeeded);
        assert!(content.contains("count"), "{content}");
    }

    /// One retyped call's result, for the tests reading its repair notes.
    async fn typed_result(input: Value) -> Event {
        typed_tool().invoke(input, &test_ctx()).await
    }

    #[tokio::test]
    async fn a_retyped_argument_is_noted_on_the_result() {
        let result = typed_result(serde_json::json!({"count": "3"})).await;
        assert_eq!(result.repairs().collect::<Vec<_>>(), vec!["/count retyped"]);

        // No pointer names a whole payload the model wrote as text.
        let result = typed_result(Value::String(r#"{"count": 3}"#.into())).await;
        assert_eq!(result.repairs().collect::<Vec<_>>(), vec!["retyped"]);
    }

    #[tokio::test]
    async fn an_argument_the_schema_does_not_declare_still_reaches_the_tool() {
        // Tool schemas close over nothing: an extra key is generosity, not error.
        let (_, succeeded) = call_typed(serde_json::json!({"count": 3, "extra": true})).await;
        assert!(succeeded);
    }

    #[tokio::test]
    async fn malformed_tool_event_becomes_a_recoverable_failure() {
        let tool = Tool::new("broken")
            .description("broken")
            .handler(|_: Value| async { Event::new("not_a_terminal_event") });
        let event = tool.invoke(serde_json::json!({}), &test_ctx()).await;
        assert_eq!(event.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(event.get_data()["kind"], "execution_failed");
        assert!(event.get_content().contains("returned an invalid event"));
    }

    #[tokio::test]
    async fn cancelling_a_run_drops_a_public_handler_future() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let marker = Arc::clone(&dropped);
        let tool = Tool::new("wait")
            .description("wait forever")
            .handler(move |_: Value| {
                let guard = Dropped(Arc::clone(&marker));
                async move {
                    let _guard = guard;
                    std::future::pending::<Event>().await
                }
            });
        let run = Arc::new(Run::default());
        let ctx = test_ctx().run(Arc::clone(&run));
        let call = tokio::spawn(async move { tool.call(serde_json::json!({}), &ctx).await });

        tokio::task::yield_now().await;
        run.set_draining(crate::agents::tasks::FinishReason::Cancelled);
        let event = call.await.unwrap();

        assert_eq!(event.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(event.get_content(), "tool call cancelled");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn malformed_terminal_metadata_becomes_a_recoverable_failure() {
        for event in [
            Event::new(Event::TOOL_CALL_FINISHED)
                .data(serde_json::json!({"output": "ok", "output_path": 42})),
            Event::new(Event::TOOL_CALL_FINISHED)
                .data(serde_json::json!({"output": "ok", "repairs": ["valid", 42]})),
            Event::new(Event::TOOL_CALL_FAILED)
                .data(serde_json::json!({"message": "no", "repairs": ["valid", 42]})),
        ] {
            let event = validate_tool_event("broken", event);
            assert_eq!(event.get_name(), Event::TOOL_CALL_FAILED);
            assert!(event.get_content().contains("returned an invalid event"));
        }
    }

    #[test]
    fn legacy_failure_reason_is_rejected() {
        let event = validate_tool_event(
            "old",
            Event::new(Event::TOOL_CALL_FAILED)
                .data(serde_json::json!({"reason": "not_found", "message": "missing"})),
        );

        assert_eq!(event.get_data()["kind"], "execution_failed");
        assert!(event.get_content().contains("returned an invalid event"));
        assert!(event.get_data().get("reason").is_none());
    }

    #[test]
    fn valid_handler_repairs_survive_but_output_path_is_runtime_owned() {
        let event = validate_tool_event(
            "custom",
            Event::new(Event::TOOL_CALL_FINISHED).data(serde_json::json!({
                "output": "ok",
                "output_path": "claimed.txt",
                "repairs": ["tool repaired its result"],
            })),
        );

        assert_eq!(event.output_path(), None);
        assert_eq!(
            event.repairs().collect::<Vec<_>>(),
            vec!["tool repaired its result"]
        );
    }

    #[test]
    fn empty_handler_repairs_are_omitted() {
        let event = validate_tool_event(
            "custom",
            Event::new(Event::TOOL_CALL_FINISHED)
                .data(serde_json::json!({"output": "ok", "repairs": []})),
        );

        assert!(event.get_data().get("repairs").is_none());
    }

    #[tokio::test]
    async fn a_typed_handler_that_cannot_read_its_arguments_names_the_author() {
        // The default schema accepts `{}`, so the mismatch is between the
        // schema and the handler's argument type: the author's mistake.
        #[derive(serde::Deserialize)]
        struct Args {
            #[allow(dead_code)]
            count: i64,
        }
        let tool = Tool::new("typed")
            .description("typed")
            .handler(|_: Args| async move { Event::success("ran") });
        let outcome = tool.call(serde_json::json!({}), &test_ctx()).await;
        assert!(
            outcome.get_name() == Event::TOOL_CALL_FAILED
                && outcome
                    .get_content()
                    .contains("`typed` could not read its arguments"),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_cloned_tool_runs_the_same_handler() {
        // The bindings register one tool on several agents; the clones must
        // share the handler, not lose it.
        let tool =
            Tool::new("echo")
                .description("Echoes input")
                .handler(|input: Value| async move {
                    Event::success(input["text"].as_str().unwrap_or("").to_string())
                });
        let cloned = tool.clone();
        let outcome = cloned
            .call(serde_json::json!({"text": "hi"}), &test_ctx())
            .await;
        assert_eq!(outcome.get_content(), "hi");
        assert_eq!(cloned.get_name(), tool.get_name());
    }

    #[test]
    fn tool_builder_preserves_name_schema_and_concurrency() {
        let tool = Tool::new("echo")
            .description("Echoes input")
            .schema(
                serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            )
            .concurrent(true)
            .handler(|input: Value| async move {
                let text = input["text"].as_str().unwrap_or("").to_string();
                Event::success(text)
            });

        assert_eq!(tool.get_name(), "echo");
        assert!(tool.is_concurrent());
    }

    // Layer 1: result-cap helpers

    fn task_ctx() -> (ToolContext, Arc<Werk>, String, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        werk.add_task("seed");
        let id = "t-1".to_string();
        let ctx = test_ctx().werk(Arc::clone(&werk)).task_id(id.clone());
        (ctx, werk, id, dir)
    }

    /// Where a capped result says its original went.
    fn offloaded_path(result: &Event) -> Option<PathBuf> {
        result.output_path().map(PathBuf::from)
    }

    /// The call an aggregate test answers with a synthetic result.
    fn sized_call(id: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.into(),
            name: "dump".into(),
            input: serde_json::json!({}),
        }
    }

    fn relative_outputs_path(task_id: &str, tool_use_id: &str) -> PathBuf {
        PathBuf::from("tasks")
            .join(task_id)
            .join("outputs")
            .join(format!("{tool_use_id}.txt"))
    }

    fn absolute_outputs_path(dir: &std::path::Path, task_id: &str, tool_use_id: &str) -> PathBuf {
        dir.join(relative_outputs_path(task_id, tool_use_id))
    }

    #[test]
    fn write_tool_output_stores_relative_path_in_comment() {
        let (ctx, _werk, id, _dir) = task_ctx();
        let mut result = Event::success("z".repeat(500));
        cap_oversized_result(&mut result, &ctx, "call-rel", 100);
        let stored = offloaded_path(&result).expect("offload happened");
        assert_eq!(stored, relative_outputs_path(&id, "call-rel"));
        assert!(
            stored.is_relative(),
            "comment path must stay portable: {}",
            stored.display()
        );
    }

    #[test]
    fn persisted_output_renders_absolute_path_for_model() {
        let (ctx, _werk, id, dir) = task_ctx();
        let mut result = Event::success("y".repeat(500));
        cap_oversized_result(&mut result, &ctx, "call-abs", 100);
        let absolute = absolute_outputs_path(dir.path(), &id, "call-abs");
        assert!(
            result
                .get_content()
                .contains(&absolute.display().to_string()),
            "stub must give the model the joinable on-disk path: {}",
            result.get_content()
        );
    }

    #[test]
    fn cap_oversized_result_passes_through_under_cap() {
        let ctx = test_ctx();
        let mut result = Event::success("hello");
        cap_oversized_result(&mut result, &ctx, "call-1", 100);
        assert_eq!(result.get_content(), "hello");
        assert!(offloaded_path(&result).is_none());
    }

    #[test]
    fn cap_oversized_result_replaces_oversized_ok_with_stub() {
        let (ctx, _werk, id, dir) = task_ctx();
        let mut result = Event::success("a".repeat(500));
        cap_oversized_result(&mut result, &ctx, "call-xyz", 100);
        let stub = result.get_content();
        assert!(stub.starts_with("<persisted-output>"));
        assert!(stub.contains("Output too large"));
        assert!(stub.contains("Full output saved to:"));
        let absolute = absolute_outputs_path(dir.path(), &id, "call-xyz");
        assert!(
            stub.contains(&absolute.display().to_string()),
            "stub must name the absolute path so the model can read the file: {stub}"
        );
        assert!(stub.contains("Preview (first"));
        assert!(stub.ends_with("</persisted-output>"));
        let path = offloaded_path(&result).expect("offload path");
        assert_eq!(path, relative_outputs_path(&id, "call-xyz"));
        let body = std::fs::read_to_string(&absolute).unwrap();
        assert_eq!(body, "a".repeat(500));
    }

    #[test]
    fn cap_oversized_result_passes_a_failure_through() {
        let ctx = test_ctx();
        let mut result = Event::error("boom");
        cap_oversized_result(&mut result, &ctx, "call-1", 1);
        assert_eq!(result.get_content(), "boom");
        assert!(offloaded_path(&result).is_none());
    }

    #[test]
    fn cap_oversized_result_returns_raw_when_no_task_id() {
        let ctx = test_ctx();
        let payload = "x".repeat(500);
        let mut result = Event::success(payload.clone());
        cap_oversized_result(&mut result, &ctx, "call-1", 100);
        assert_eq!(result.get_content(), payload);
        assert!(
            offloaded_path(&result).is_none(),
            "no task ID means no offload"
        );
    }

    #[test]
    fn cap_aggregate_offloads_largest_first() {
        let (ctx, _werk, id, dir) = task_ctx();
        // Sizes chosen so the stub's own bytes (~200) don't dominate.
        let big = "b".repeat(80_000);
        let calls = vec![sized_call("c1"), sized_call("c2"), sized_call("c3")];
        let mut results = vec![
            Event::success("a".repeat(40_000)),
            Event::success(big.clone()),
            Event::success("c".repeat(30_000)),
        ];
        cap_aggregate_outputs(&calls, &mut results, &ctx, 100_000);
        // c2 (the largest) was offloaded; the other two stayed inline.
        assert!(results[1].get_content().starts_with("<persisted-output>"));
        assert!(results[1].get_content().contains("Full output saved to:"));
        assert_eq!(
            offloaded_path(&results[1]),
            Some(relative_outputs_path(&id, "c2"))
        );
        let body = std::fs::read_to_string(absolute_outputs_path(dir.path(), &id, "c2")).unwrap();
        assert_eq!(body, big);

        assert_eq!(results[0].get_content().len(), 40_000);
        assert_eq!(results[2].get_content().len(), 30_000);
        assert!(offloaded_path(&results[0]).is_none());
        assert!(offloaded_path(&results[2]).is_none());
    }

    #[test]
    fn cap_aggregate_stops_when_only_small_results_remain() {
        let (ctx, _werk, _key, _dir) = task_ctx();
        // Many small results whose total far exceeds the cap, but
        // each is already stub-marked. Aggregate should bail
        // rather than spin: stubs are skipped, so no candidates.
        let calls: Vec<ContentBlock> = (0..5).map(|i| sized_call(&format!("c{i}"))).collect();
        let mut results: Vec<Event> = (0..5)
            .map(|i| {
                Event::success(format!(
                    "<persisted-output>already stubbed {i}</persisted-output>"
                ))
            })
            .collect();
        let before: Vec<String> = results
            .iter()
            .map(|r| r.get_content().to_string())
            .collect();
        cap_aggregate_outputs(&calls, &mut results, &ctx, 10);
        let after: Vec<String> = results
            .iter()
            .map(|r| r.get_content().to_string())
            .collect();
        assert_eq!(
            before, after,
            "aggregate must be a no-op when only stubs remain"
        );
        assert!(results.iter().all(|r| offloaded_path(r).is_none()));
    }

    #[test]
    fn format_oversized_tool_result_renders_template() {
        let path = PathBuf::from("/tmp/agentwerk/tasks/t-1/outputs/call-1.txt");
        let stub = format_oversized_tool_result(
            1_048_576,
            &path,
            "preview-body",
            &DirectiveStore::default(),
        );
        assert!(stub.starts_with("<persisted-output>"));
        assert!(stub.contains("Output too large (1.0 MB)."));
        assert!(stub.contains("Full output saved to: /tmp/agentwerk/tasks/t-1/outputs/call-1.txt"));
        assert!(stub.contains("Preview (first 12 B):"));
        assert!(stub.contains("preview-body"));
        assert!(stub.ends_with("</persisted-output>"));
    }

    #[test]
    fn truncate_preview_snaps_at_last_newline_in_window() {
        let mut content = String::new();
        // Build a payload where the last newline within PREVIEW_CHARS is
        // at byte 1_900.
        content.push_str(&"a".repeat(1_900));
        content.push('\n');
        content.push_str(&"b".repeat(500));
        let preview = truncate_preview(&content);
        assert_eq!(preview.len(), 1_901);
        assert!(preview.ends_with('\n'));
    }

    #[test]
    fn truncate_preview_falls_back_to_utf8_boundary_when_no_newline() {
        let content = "x".repeat(3_000);
        let preview = truncate_preview(&content);
        assert_eq!(preview.len(), PREVIEW_CHARS);
        assert!(content.is_char_boundary(preview.len()));
    }

    #[test]
    fn truncate_preview_does_not_split_a_multibyte_char_at_the_window() {
        // A 3-byte char straddling PREVIEW_CHARS must not be sliced through: the
        // window floors to the char boundary below 2000 instead of panicking.
        let mut content = "x".repeat(PREVIEW_CHARS - 1);
        content.push('世'); // occupies bytes 1999..2002, crossing the 2000 window
        content.push_str(&"y".repeat(500));
        let preview = truncate_preview(&content);
        assert!(content.is_char_boundary(preview.len()));
        assert!(preview.len() <= PREVIEW_CHARS);
    }

    #[test]
    fn replace_empty_output_substitutes_placeholder() {
        let mut result = Event::success("");
        replace_empty_output(&mut result, "bash", &DirectiveStore::default());
        assert_eq!(result.get_content(), "(bash completed with no output)");
    }

    #[test]
    fn replace_empty_output_passes_non_empty_through() {
        let mut result = Event::success("hello");
        replace_empty_output(&mut result, "bash", &DirectiveStore::default());
        assert_eq!(result.get_content(), "hello");
    }

    #[test]
    fn replace_empty_output_passes_a_failure_through() {
        // The guard reads the variant, not the content, so even an empty
        // failure message is left as the tool reported it.
        let mut result = Event::error("");
        replace_empty_output(&mut result, "bash", &DirectiveStore::default());
        assert_eq!(result.get_content(), "");
    }

    #[test]
    fn cap_aggregate_skips_a_failed_result() {
        // A failure's message is what the model must read to recover, so the
        // aggregate cap never writes it out.
        let (ctx, _werk, _key, _dir) = task_ctx();
        let calls = vec![sized_call("c1")];
        let mut results = vec![Event::error("e".repeat(50_000))];
        cap_aggregate_outputs(&calls, &mut results, &ctx, 10);
        assert_eq!(results[0].get_content().len(), 50_000);
        assert!(offloaded_path(&results[0]).is_none());
    }
}

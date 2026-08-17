//! The actions agents can take, and the registry an agent's tools live in.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::knowledge::Knowledge;
use crate::agents::tickets::{Run, TicketQueue};
use crate::event::{EventKind, RepairKind, ToolFailureKind};
use crate::providers::ToolDefinition;
use crate::schemas::Schema;

/// The largest result one tool may return. Anything longer is written to
/// `<ticket-dir>/outputs/<tool_use_id>.txt` and replaced with a short stub.
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

/// What a tool receives when it runs.
///
/// Writing your own tool, you need two of these:
/// - `dir`: the agent's working directory.
/// - `cancelled()`: resolves when the current call is given up on.
///
/// agentwerk fills in the rest for the built-in tools.
#[derive(Clone)]
pub struct ToolContext {
    /// Directory the tool runs in. Resolve a relative path against it.
    pub dir: PathBuf,
    pub(crate) run: Option<Arc<Run>>,
    pub(crate) ticket_queue: Option<Arc<TicketQueue>>,
    pub(crate) agent_id: Option<String>,
    pub(crate) ticket_key: Option<String>,
    pub(crate) knowledge: Option<Arc<Knowledge>>,
}

impl ToolContext {
    /// A context rooted at `dir` that is never cancelled. Use it standalone or
    /// in tests; agentwerk installs its own at call time.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            run: None,
            ticket_queue: None,
            agent_id: None,
            ticket_key: None,
            knowledge: None,
        }
    }

    pub(crate) fn run(mut self, run: Arc<Run>) -> Self {
        self.run = Some(run);
        self
    }

    pub(crate) fn ticket_queue(mut self, queue: Arc<TicketQueue>) -> Self {
        self.ticket_queue = Some(queue);
        self
    }

    pub(crate) fn agent_id(mut self, name: String) -> Self {
        self.agent_id = Some(name);
        self
    }

    pub(crate) fn ticket_key(mut self, key: String) -> Self {
        self.ticket_key = Some(key);
        self
    }

    pub(crate) fn knowledge(mut self, knowledge: Arc<Knowledge>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Publish `kind` for the ticket and agent this call runs for. A context
    /// with no queue publishes nothing; the call still runs.
    pub(crate) fn emit(&self, kind: EventKind) {
        let Some(queue) = &self.ticket_queue else {
            return;
        };
        let key = self.ticket_key.as_deref().unwrap_or_default();
        let agent = self.agent_id.as_deref().unwrap_or_default();
        queue.emit(key, agent, kind);
    }

    /// Resolves once the run starts to finish, whether the caller cancelled it
    /// or a limit was breached: either way this call is being given up on.
    ///
    /// Pair it with `tokio::select!` so the losing branch is dropped, which
    /// aborts an in-flight HTTP request and, with `kill_on_drop(true)`, ends a
    /// subprocess. On a standalone context it never resolves, and the `select!`
    /// behaves like a plain await.
    pub async fn cancelled(&self) {
        match &self.run {
            Some(run) => run.until_draining().await,
            None => std::future::pending::<()>().await,
        }
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("dir", &self.dir)
            .field("has_ticket_queue", &self.ticket_queue.is_some())
            .finish()
    }
}

/// One tool call the model asked for.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Identifier for this call, sent back with the result.
    pub id: String,
    /// Name of the tool the model chose.
    pub name: String,
    /// Arguments the model supplied, matching the tool's input schema.
    pub input: Value,
}

/// What a tool reports back: a result, an error the model should work around,
/// or arguments that missed the tool's own schema.
///
/// All three reach the model the same way, and both failures count against
/// `max_schema_retries`. [`SchemaError`] alone also shows the model the schema
/// this tool advertised, with a directive to match it.
///
/// [`SchemaError`]: ToolResult::SchemaError
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResult {
    /// The tool ran and produced this text.
    Success(String),
    /// The tool failed, and the message tells the model how to recover.
    Error(String),
    /// The arguments missed the tool's schema, and the message names how.
    SchemaError(String),
}

impl ToolResult {
    /// Report a result.
    pub fn success(content: impl Into<String>) -> Self {
        Self::Success(content.into())
    }

    /// Report a failure the model should work around.
    pub fn error(content: impl Into<String>) -> Self {
        Self::Error(content.into())
    }

    /// Report arguments that missed a schema, worded as
    /// [`Schema::validate`](crate::Schema::validate) words it. Reach for this
    /// when your tool runs a schema of its own; agentwerk already checks the
    /// one the tool declares.
    ///
    /// The model reads that schema back with a directive to match it, which
    /// misleads when the rule broken is one no schema states. Report those with
    /// [`error`](Self::error), saying what to do instead. Both count against
    /// `max_schema_retries`.
    pub fn schema_error(content: impl Into<String>) -> Self {
        Self::SchemaError(content.into())
    }

    /// Get the text, whichever outcome this is.
    pub fn content(&self) -> &str {
        let (Self::Success(content) | Self::Error(content) | Self::SchemaError(content)) = self;
        content
    }
}

/// The tools one agent may call.
#[derive(Clone, Default)]
pub(crate) struct ToolRegistry {
    tools: Vec<Arc<Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.tools.iter().map(|tool| tool.name()).collect();
        f.debug_struct("ToolRegistry")
            .field("tools", &names)
            .finish()
    }
}

impl ToolRegistry {
    /// Register a tool, replacing one already registered under that name.
    ///
    /// A request carrying one name twice is rejected, so the last registration
    /// is the one that counts.
    pub(crate) fn register(&mut self, tool: impl Into<Tool>) {
        let tool = Arc::new(tool.into());
        self.tools.retain(|t| t.name() != tool.name());
        self.tools.push(tool);
    }

    /// Get the arguments `name` advertised for `ticket`, which is what a call
    /// this registry rejected reads back.
    ///
    /// Resolved through the same spelling fold as [`get`](Self::get), so a folded
    /// name reads back the schema of the tool that would have run.
    pub(crate) fn advertised_schema(&self, name: &str, ticket: Option<&Schema>) -> Option<Value> {
        let tool = self.get(name)?;
        Some(advertised(&tool, ticket))
    }

    /// Get the tool a call reaches, owned, so a concurrent batch can move it
    /// into its task, or the message naming what could have been called.
    fn resolve(&self, name: &str) -> std::result::Result<Arc<Tool>, String> {
        self.get(name).ok_or_else(|| {
            let names = self.names();
            if names.is_empty() {
                format!("Unknown tool: {name}")
            } else {
                format!(
                    "Unknown tool: {name}. Available tools: {}",
                    names.join(", ")
                )
            }
        })
    }

    /// Get the tool a call names.
    ///
    /// An exact match wins. Otherwise a spelling that reduces to the same key as
    /// exactly one registered tool resolves to it, so a model that adds a
    /// `_tool` suffix still reaches the right tool.
    pub(crate) fn get(&self, name: &str) -> Option<Arc<Tool>> {
        let name = name.trim();
        if let Some(found) = self.tools.iter().find(|tool| tool.name() == name) {
            return Some(Arc::clone(found));
        }
        let key = lookup_key(name);
        let mut folded = self
            .tools
            .iter()
            .filter(|tool| lookup_key(tool.name()) == key);
        let found = folded.next()?;
        // A key two tools share is ambiguous: refuse rather than guess.
        folded.next().is_none().then(|| Arc::clone(found))
    }

    /// Get the registered names, sorted, for the error that tells the model what
    /// it could have called.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        names.sort();
        names
    }

    /// Get the tool definitions sent to the model, the finish tool's arguments
    /// carrying `ticket`'s schema.
    pub(crate) fn definitions(&self, ticket: Option<&Schema>) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|tool| ToolDefinition {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: advertised(tool, ticket),
            })
            .collect()
    }

    /// Run the calls, concurrent ones together and the rest one at a time.
    pub(crate) async fn execute(&self, calls: &[ToolCall], ctx: &ToolContext) -> Vec<ToolOutcome> {
        let batches = partition_tool_calls(calls, self);
        let mut results: Vec<ToolOutcome> = Vec::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(10));

        for batch in batches {
            match batch {
                ToolBatch::Concurrent(calls) => {
                    let mut set = tokio::task::JoinSet::new();
                    for call in calls {
                        let sem = semaphore.clone();
                        let ctx = ctx.clone();
                        // Resolved before the spawn: the task outlives this
                        // borrow of the registry.
                        let found = self.resolve(&call.name);
                        set.spawn(async move {
                            let _permit = sem.acquire().await.unwrap();
                            run_call(found, &call, &ctx).await
                        });
                    }

                    while let Some(join_result) = set.join_next().await {
                        if let Ok(outcome) = join_result {
                            results.push(outcome);
                        }
                    }
                }
                ToolBatch::Serial(call) => {
                    results.push(run_call(self.resolve(&call.name), &call, ctx).await);
                }
            }
        }

        cap_aggregate_outputs(&mut results, ctx, PER_TURN_CAP);

        results
    }
}

/// Compose the arguments `tool` shows the model for `ticket`. Only the finish
/// tool's depend on it; every other tool shows what it registered.
///
/// One answer serves the definitions the model is shown and the shape a
/// rejected call reads back, so the two cannot disagree.
fn advertised(tool: &Tool, ticket: Option<&Schema>) -> Value {
    match ticket.filter(|_| tool.name() == super::FinishTool::name()) {
        Some(ticket) => super::FinishTool::input_schema_for(ticket),
        None => document(tool),
    }
}

/// The JSON document `tool` declares, for the definition sent to the model.
fn document(tool: &Tool) -> Value {
    tool.input_schema().get_raw_schema().clone()
}

/// Reduce a tool name to the key lookups match on, so capitalization, hyphens,
/// or a trailing `_tool` still reach the tool.
///
/// The reduction only removes information, so it cannot invent a name nobody
/// asked for. Where two tools share a key, [`ToolRegistry::get`] refuses.
fn lookup_key(name: &str) -> String {
    let key = name.trim().to_lowercase().replace('-', "_");
    match key.strip_suffix("_tool") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => key,
    }
}

type ToolHandler = Arc<
    dyn Fn(Value, &ToolContext) -> Pin<Box<dyn Future<Output = ToolResult> + Send + '_>>
        + Send
        + Sync,
>;

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
/// use agentwerk::tools::{Tool, ToolResult};
/// use serde_json::{json, Value};
///
/// let greet = Tool::new("greet", "Say hello to a name.", |input: Value, _ctx| async move {
///     let name = input["name"].as_str().unwrap_or("world");
///     ToolResult::success(format!("Hello, {name}!"))
/// })
/// .schema(json!({
///     "type": "object",
///     "properties": { "name": { "type": "string" } },
///     "required": ["name"]
/// }))
/// .concurrent(true);
///
/// Agent::new().tool(greet);
/// ```
#[derive(Clone)]
pub struct Tool {
    name: String,
    description: String,
    schema: Schema,
    concurrent: bool,
    paths: Vec<String>,
    handler: ToolHandler,
}

impl Tool {
    /// Create a tool from its name, its description, and the handler that runs
    /// when the model calls it. A bare `async` block works. Refine what the
    /// tool accepts with [`schema`](Self::schema).
    pub fn new<A, F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: F,
    ) -> Tool
    where
        A: serde::de::DeserializeOwned + Send + 'static,
        F: Fn(A, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        let name = name.into();
        Tool {
            handler: read_arguments_then(name.clone(), handler),
            name,
            description: description.into(),
            schema: Schema::new(serde_json::json!({"type": "object", "properties": {}}))
                .expect("a literal object schema compiles"),
            concurrent: false,
            paths: Vec::new(),
        }
    }

    /// Create a tool from a `.tool.md` definition, the JSON Schema document
    /// beside it, and the handler that runs. The definition supplies the name,
    /// description, and whether the tool may run concurrently. Panics when
    /// either file is malformed or the schema does not compile.
    pub fn from_tool_file<A, F, Fut>(definition: &str, schema: &str, handler: F) -> Tool
    where
        A: serde::de::DeserializeOwned + Send + 'static,
        F: Fn(A, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        let tf = super::tool_file::ToolFile::parse(definition, schema);
        Tool {
            handler: read_arguments_then(tf.name.clone(), handler),
            description: tf.render_markdown(),
            schema: tf.input_schema.clone(),
            concurrent: tf.concurrent,
            paths: Vec::new(),
            name: tf.name,
        }
    }

    /// Define what the tool accepts, as JSON Schema. Panics on a document
    /// `Schema::new` refuses, naming this tool: an uncheckable tool is a
    /// mistake here, not one the agent should discover at call time.
    pub fn schema(mut self, schema: Value) -> Tool {
        self.schema = Schema::new(schema).unwrap_or_else(|error| {
            panic!(
                "tool `{}` declares a schema that does not compile: {error}",
                self.name
            )
        });
        self
    }

    /// Run this tool in parallel with the turn's other concurrent calls. Set it
    /// for a tool with no side effects.
    pub fn concurrent(mut self, concurrent: bool) -> Tool {
        self.concurrent = concurrent;
        self
    }

    /// Name the input fields holding a file path, so the files a call opens are
    /// included in statistics. A field that is absent or not a string is
    /// skipped.
    pub fn paths<I, S>(mut self, fields: I) -> Tool
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.paths = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Run the tool on a call the registry has already checked against
    /// [`schema`](Self::schema).
    pub async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        (self.handler)(input, ctx).await
    }

    /// The name the model calls the tool by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the tool does, in the words the model reads.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The arguments this tool accepts, compiled.
    pub fn input_schema(&self) -> &Schema {
        &self.schema
    }

    /// Whether the agent may run this tool alongside the turn's other
    /// concurrent calls.
    pub fn is_concurrent(&self) -> bool {
        self.concurrent
    }

    /// The file paths this call opens, read from the fields named by
    /// [`paths`](Self::paths), so they reach `Stats`.
    pub fn opened_paths(&self, input: &Value) -> Vec<String> {
        self.paths
            .iter()
            .filter_map(|field| input.get(field).and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect()
    }
}

/// Fold a typed handler into the stored form: deserialize the checked
/// arguments, then run the handler on what they read as.
fn read_arguments_then<A, F, Fut>(name: String, handler: F) -> ToolHandler
where
    A: serde::de::DeserializeOwned + Send + 'static,
    F: Fn(A, ToolContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ToolResult> + Send + 'static,
{
    Arc::new(move |input, ctx| match serde_json::from_value::<A>(input) {
        Ok(args) => Box::pin(handler(args, ctx.clone())),
        // The schema accepted this call, so the tool's schema and its
        // argument type disagree: the author's mistake, not the model's.
        // Reporting it as a schema failure would show the model a document
        // its call already satisfied.
        Err(error) => Box::pin(std::future::ready(ToolResult::error(format!(
            "`{name}` could not read its arguments: {error}"
        )))),
    })
}

/// What one call produced, collected by the turn.
pub(crate) struct ToolOutcome {
    /// Identifier of the call, sent back with the result.
    pub(crate) call_id: String,
    /// What the model reads back: the tool's output, or the failure message.
    pub(crate) content: String,
    /// How the call failed. `None` is a success.
    pub(crate) failure: Option<ToolFailureKind>,
    /// Where an oversized output went, relative to the session directory.
    pub(crate) path: Option<PathBuf>,
}

/// Run one call to the outcome the turn collects: check the arguments, run the
/// tool, then stand in for an empty or oversized result.
async fn run_call(
    found: std::result::Result<Arc<Tool>, String>,
    call: &ToolCall,
    ctx: &ToolContext,
) -> ToolOutcome {
    let mut outcome = invoke(found, call, ctx).await;
    replace_empty_output(&mut outcome, &call.name);
    cap_oversized_result(&mut outcome, ctx, PER_TOOL_CAP);
    outcome
}

/// Check the arguments against the schema the tool registered, then run it on
/// what survives.
async fn invoke(
    resolved: std::result::Result<Arc<Tool>, String>,
    call: &ToolCall,
    ctx: &ToolContext,
) -> ToolOutcome {
    let outcome = |content: String, failure: Option<ToolFailureKind>| ToolOutcome {
        call_id: call.id.clone(),
        content,
        failure,
        path: None,
    };
    let tool = match resolved {
        Ok(tool) => tool,
        Err(message) => return outcome(message, Some(ToolFailureKind::ToolNotFound)),
    };
    // Retyped rather than refused, so a quoted number runs the call the model
    // asked for and arguments it wrote as JSON text are decoded. What comes
    // back names the value that produced, which is the one the tool would have
    // received.
    let (input, repairs) = match tool.input_schema().validate(call.input.clone()) {
        Ok(validated) => validated,
        Err(violations) => {
            return outcome(
                violations.to_string(),
                Some(ToolFailureKind::SchemaValidationFailed),
            );
        }
    };
    for pointer in &repairs {
        ctx.emit(EventKind::ResponseRepaired {
            tool_name: call.name.clone(),
            reason: RepairKind::ValueMistyped,
            message: retype_message(pointer),
        });
    }
    match tool.call(input, ctx).await {
        ToolResult::Success(content) => outcome(content, None),
        ToolResult::Error(content) => outcome(content, Some(ToolFailureKind::ExecutionFailed)),
        ToolResult::SchemaError(content) => {
            outcome(content, Some(ToolFailureKind::SchemaValidationFailed))
        }
    }
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

/// Put a placeholder in place of an empty result, since empty content has upset
/// LLM providers. A failure passes through: its message is never empty.
fn replace_empty_output(outcome: &mut ToolOutcome, tool_name: &str) {
    if outcome.failure.is_none() && outcome.content.is_empty() {
        outcome.content = format!("({tool_name} completed with no output)");
    }
}

enum ToolBatch {
    Concurrent(Vec<ToolCall>),
    Serial(ToolCall),
}

fn partition_tool_calls(calls: &[ToolCall], registry: &ToolRegistry) -> Vec<ToolBatch> {
    let mut batches: Vec<ToolBatch> = Vec::new();
    let mut concurrent_batch: Vec<ToolCall> = Vec::new();

    for call in calls {
        let is_concurrent = registry.get(&call.name).is_some_and(|t| t.is_concurrent());

        if is_concurrent {
            concurrent_batch.push(call.clone());
        } else {
            if !concurrent_batch.is_empty() {
                batches.push(ToolBatch::Concurrent(std::mem::take(&mut concurrent_batch)));
            }
            batches.push(ToolBatch::Serial(call.clone()));
        }
    }

    if !concurrent_batch.is_empty() {
        batches.push(ToolBatch::Concurrent(concurrent_batch));
    }

    batches
}

/// Replace an oversized result with a stub, writing the original under the
/// ticket's outputs directory and recording where it went.
///
/// A failure passes through, being short by construction, and so does the raw
/// content when the write fails.
fn cap_oversized_result(outcome: &mut ToolOutcome, ctx: &ToolContext, per_tool_cap: usize) {
    if outcome.failure.is_some() || outcome.content.len() <= per_tool_cap {
        return;
    }
    let Some(p) = persist_output(ctx, &outcome.call_id, &outcome.content) else {
        return;
    };
    let preview = truncate_preview(&outcome.content);
    let stub = format_oversized_tool_result(outcome.content.len(), &p.display, preview);
    outcome.content = stub;
    outcome.path = Some(p.rel);
}

/// While one turn's results are too large together, write out the largest that
/// is not already a stub. It stops once the turn fits, or once nothing left can
/// be written out. A failure is never written out: its message is short by
/// construction, the same rule the per-call cap applies.
fn cap_aggregate_outputs(results: &mut [ToolOutcome], ctx: &ToolContext, per_turn_cap: usize) {
    loop {
        let total: usize = results.iter().map(|outcome| outcome.content.len()).sum();
        if total <= per_turn_cap {
            return;
        }
        let largest = results
            .iter_mut()
            .filter(|outcome| outcome.failure.is_none())
            .filter(|outcome| !outcome.content.starts_with(OVERSIZED_STUB_TAG_OPEN))
            .max_by_key(|outcome| outcome.content.len());
        let Some(outcome) = largest else {
            return;
        };
        let Some(p) = persist_output(ctx, &outcome.call_id, &outcome.content) else {
            // Persistence failed; nothing further this pass can do.
            return;
        };
        let preview = truncate_preview(&outcome.content);
        let stub = format_oversized_tool_result(outcome.content.len(), &p.display, preview);
        outcome.content = stub;
        outcome.path = Some(p.rel);
    }
}

/// Write `content` under the ticket's outputs directory, reporting both the
/// path relative to the session and the path on disk.
///
/// `None` when the context names no ticket, no ticket queue is attached, or
/// the write fails. Like the rest of the logging, it is best effort.
fn persist_output(ctx: &ToolContext, tool_use_id: &str, content: &str) -> Option<PersistedOutput> {
    let queue = ctx.ticket_queue.as_ref()?;
    let key = ctx.ticket_key.as_deref()?;
    let rel = queue.write_tool_output(key, tool_use_id, content)?;
    let display = queue.get_dir().join(&rel);
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
fn format_oversized_tool_result(original_len: usize, path: &Path, preview: &str) -> String {
    let size = format_bytes(original_len);
    let preview_size = format_bytes(preview.len());
    format!(
        "{OVERSIZED_STUB_TAG_OPEN}Output too large ({size}). Full output saved to: {path}\n\
         Preview (first {preview_size}):\n\
         {preview}\n\
         {OVERSIZED_STUB_TAG_CLOSE}",
        path = path.display(),
    )
}

/// The first `PREVIEW_CHARS` bytes of `content`, ending at the last newline in
/// that window or at a character boundary.
///
/// The window is moved to a character boundary first: `PREVIEW_CHARS` can land
/// inside a multi-byte character, and slicing there would panic.
fn truncate_preview(content: &str) -> &str {
    let window = utf8_boundary_floor(content, PREVIEW_CHARS.min(content.len()));
    let cut = content[..window]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(window);
    &content[..cut]
}

fn format_bytes(n: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if n < 1024 {
        format!("{n} B")
    } else if (n as f64) < MB {
        format!("{:.1} KB", n as f64 / KB)
    } else {
        format!("{:.1} MB", n as f64 / MB)
    }
}

/// Move an index back to the nearest character boundary, at most three bytes.
fn utf8_boundary_floor(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

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
            crate::tools::FetchUrlTool.into(),
            crate::tools::KnowledgeTool::new(store).into(),
            crate::tools::CommandTool::new("git").allow("git *").into(),
            crate::tools::FinishTool.into(),
            crate::tools::TicketsTool.into(),
        ];
        (tools, dir)
    }

    /// Every built-in `.tool.md` must parse. Compiling is now structural, since
    /// `ToolFile::parse` panics on a fence `Schema::new` refuses, so what is
    /// left to force here is the name, the description, and the object type.
    #[test]
    fn every_built_in_tool_definition_parses() {
        let (tools, _dir) = built_in_tools();
        for tool in &tools {
            assert!(!tool.name().is_empty(), "tool name is empty");
            assert!(
                !tool.description().is_empty(),
                "empty description for {}",
                tool.name(),
            );
            // The registry holds a call to this, so a tool that declares
            // something else loses the check that its arguments are an object.
            assert_eq!(
                tool.input_schema().get_raw_schema()["type"],
                "object",
                "arguments are not an object for {}",
                tool.name(),
            );
        }
    }

    #[test]
    fn every_example_a_built_in_tool_shows_is_a_call_its_own_schema_accepts() {
        // An example needing a repair, or failing outright, teaches the model a
        // shape it will be corrected for.
        let (tools, _dir) = built_in_tools();
        for tool in &tools {
            let schema = tool.input_schema();
            let examples = schema.get_raw_schema()["examples"]
                .as_array()
                .unwrap_or_else(|| panic!("{} shows no examples", tool.name()))
                .clone();
            for example in examples {
                let (_, repaired) = schema
                    .validate(example.clone())
                    .unwrap_or_else(|violations| panic!("{}: {violations}", tool.name()));
                assert!(repaired.is_empty(), "{} repaired {example}", tool.name());
            }
        }
    }

    #[test]
    fn registering_a_name_twice_leaves_the_later_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("echo", true, "first"));
        registry.register(mock_tool("echo", true, "second"));

        let definitions = registry.definitions(None);
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "echo");
    }

    #[test]
    fn paths_reports_the_named_input_fields() {
        let tool = Tool::new("cat", "Read a file.", |_: Value, _ctx| async move {
            ToolResult::success("ok")
        })
        .paths(["path", "into"]);

        let input = serde_json::json!({"path": "src/lib.rs", "limit": 20});
        assert_eq!(tool.opened_paths(&input), vec!["src/lib.rs".to_string()]);
    }

    /// The mock the registry tests share.
    fn mock_tool(name: &str, concurrent: bool, result: &str) -> Tool {
        let result = result.to_string();
        Tool::new(name, "mock", move |_: Value, _ctx| {
            let result = result.clone();
            async move { ToolResult::success(result) }
        })
        .schema(serde_json::json!({"type": "object"}))
        .concurrent(concurrent)
    }

    fn test_ctx() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn registry_register_and_get() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read_file", true, "file contents"));
        assert!(registry.get("read_file").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn resolves_a_name_carrying_a_tool_suffix() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "matches"));

        let tool = registry.get("grep_tool").expect("suffix should fold away");
        assert_eq!(tool.name(), "grep");
    }

    #[test]
    fn resolves_a_name_the_model_hyphenated() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read_file", true, "file contents"));

        let tool = registry
            .get("Read-File")
            .expect("case and hyphen should fold");
        assert_eq!(tool.name(), "read_file");
    }

    #[test]
    fn refuses_a_name_two_tools_share_a_key() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "builtin"));
        registry.register(mock_tool("grep_tool", true, "host tool"));

        assert!(registry.get("Grep").is_none());
        // Each registered name still reaches its own tool: exact match wins.
        assert_eq!(registry.get("grep").unwrap().name(), "grep");
        assert_eq!(registry.get("grep_tool").unwrap().name(), "grep_tool");
    }

    #[test]
    fn from_tool_file_populates_name_description_schema_concurrent() {
        let definition = r#"---
name: demo_tool
concurrent: true
---

Do the demo thing.

- Returns nothing useful.
"#;
        let schema = r#"{
  "type": "object",
  "properties": {"x": {"type": "string"}},
  "required": ["x"]
}"#;
        let tool = Tool::from_tool_file(definition, schema, |_: Value, _| async {
            ToolResult::success("")
        });
        assert_eq!(tool.name(), "demo_tool");
        assert!(tool.description().contains("Do the demo thing."));
        assert!(tool.description().contains("- Returns nothing useful."));
        assert!(tool.is_concurrent());
        let schema = tool.input_schema();
        let document = schema.get_raw_schema();
        assert_eq!(document["properties"]["x"]["type"], "string");
        assert_eq!(document["required"][0], "x");
    }

    #[test]
    fn registry_definitions() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read", true, "ok"));
        registry.register(mock_tool("write", false, "ok"));

        let defs = registry.definitions(None);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "read");
        assert_eq!(defs[1].name, "write");
    }

    #[test]
    fn a_rejected_call_reads_back_the_arguments_its_tool_advertised() {
        let mut registry = ToolRegistry::default();
        registry.register(crate::tools::FinishTool);
        let ticket = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"partial_sum": {"type": "integer"}},
            "required": ["partial_sum"],
        }))
        .unwrap();

        let definitions = registry.definitions(Some(&ticket));
        let advertised = registry.advertised_schema("finish", Some(&ticket)).unwrap();

        let shown = definitions
            .iter()
            .find(|definition| definition.name == "finish")
            .expect("finish is registered");
        assert_eq!(shown.input_schema, advertised);
        assert!(
            advertised["properties"]["partial_sum"].is_object(),
            "{advertised}"
        );
    }

    #[test]
    fn a_tool_the_ticket_says_nothing_about_advertises_what_it_registered() {
        let mut registry = ToolRegistry::default();
        registry.register(typed_tool());
        let ticket = Schema::new(serde_json::json!({"type": "string"})).unwrap();

        let advertised = registry.advertised_schema("typed", Some(&ticket)).unwrap();

        assert_eq!(&advertised, typed_tool().input_schema().get_raw_schema());
    }

    #[test]
    fn registry_clone() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("t", true, "ok"));
        let cloned = registry.clone();
        assert_eq!(cloned.definitions(None).len(), 1);
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let registry = ToolRegistry::default();
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "nonexistent".into(),
            input: serde_json::json!({}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].failure, Some(ToolFailureKind::ToolNotFound));
        assert!(results[0].content.contains("Unknown tool"));
    }

    #[tokio::test]
    async fn non_object_input_is_reported_as_such_and_never_reaches_the_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "matches"));
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "grep".into(),
            input: Value::String(r#"{"pattern": "exec""#.into()),
        }];

        let results = registry.execute(&calls, &ctx).await;
        let content = &results[0].content;
        assert_eq!(
            results[0].failure,
            Some(ToolFailureKind::SchemaValidationFailed)
        );
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
        Tool::new("typed", "typed", |input: Value, _ctx| async move {
            ToolResult::success(input["count"].to_string())
        })
        .schema(serde_json::json!({
            "type": "object",
            "properties": { "count": { "type": "integer" } },
            "required": ["count"],
        }))
    }

    async fn call_typed(input: Value) -> (String, bool) {
        call_typed_in(input, &test_ctx()).await
    }

    async fn call_typed_in(input: Value, ctx: &ToolContext) -> (String, bool) {
        let mut registry = ToolRegistry::default();
        registry.register(typed_tool());
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "typed".into(),
            input,
        }];
        let results = registry.execute(&calls, ctx).await;
        (results[0].content.clone(), results[0].failure.is_none())
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

    #[tokio::test]
    async fn a_retyped_argument_is_reported_as_a_repair() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        let seen: Arc<std::sync::Mutex<Vec<(String, RepairKind, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = Arc::clone(&seen);
        queue.on_event(move |event| {
            if let EventKind::ResponseRepaired {
                tool_name,
                reason,
                message,
            } = &event.kind
            {
                collected
                    .lock()
                    .unwrap()
                    .push((tool_name.clone(), *reason, message.clone()));
            }
        });
        let ctx = ToolContext::new(dir.path().to_path_buf())
            .ticket_queue(Arc::clone(&queue))
            .agent_id("alice".into())
            .ticket_key("TICKET-1".into());

        let (_, succeeded) = call_typed_in(serde_json::json!({"count": "3"}), &ctx).await;

        assert!(succeeded);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(
                "typed".to_string(),
                RepairKind::ValueMistyped,
                "/count retyped".to_string()
            )]
        );

        seen.lock().unwrap().clear();
        // No pointer names a whole payload the model wrote as text.
        let arguments = Value::String(r#"{"count": 3}"#.into());
        let (_, succeeded) = call_typed_in(arguments, &ctx).await;

        assert!(succeeded);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(
                "typed".to_string(),
                RepairKind::ValueMistyped,
                "retyped".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn an_argument_the_schema_does_not_declare_still_reaches_the_tool() {
        // Tool schemas close over nothing: an extra key is generosity, not error.
        let (_, succeeded) = call_typed(serde_json::json!({"count": 3, "extra": true})).await;
        assert!(succeeded);
    }

    #[tokio::test]
    async fn unknown_tool_error_names_the_registered_tools() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("grep", true, "matches"));
        registry.register(mock_tool("read_file", true, "file contents"));
        let ctx = test_ctx();
        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "ripgrep".into(),
            input: serde_json::json!({}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(
            results[0].content,
            "Unknown tool: ripgrep. Available tools: grep, read_file"
        );
    }

    #[tokio::test]
    async fn execute_concurrent_tools_at_the_same_time() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("read1", true, "result1"));
        registry.register(mock_tool("read2", true, "result2"));
        let ctx = test_ctx();

        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "read1".into(),
                input: serde_json::json!({}),
            },
            ToolCall {
                id: "c2".into(),
                name: "read2".into(),
                input: serde_json::json!({}),
            },
        ];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn execute_serial_tool() {
        let mut registry = ToolRegistry::default();
        registry.register(mock_tool("write_file", false, "written"));
        let ctx = test_ctx();

        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            input: serde_json::json!({"path": "/tmp/test"}),
        }];

        let results = registry.execute(&calls, &ctx).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].failure.is_none());
        assert_eq!(results[0].content, "written");
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
        let tool = Tool::new("typed", "typed", |_: Args, _ctx| async move {
            ToolResult::success("ran")
        });
        let outcome = tool.call(serde_json::json!({}), &test_ctx()).await;
        assert!(
            matches!(&outcome, ToolResult::Error(message)
                if message.contains("`typed` could not read its arguments")),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_cloned_tool_runs_the_same_handler() {
        // The bindings register one tool on several agents; the clones must
        // share the handler, not lose it.
        let tool = Tool::new("echo", "Echoes input", |input: Value, _ctx| async move {
            ToolResult::success(input["text"].as_str().unwrap_or("").to_string())
        });
        let cloned = tool.clone();
        let outcome = cloned
            .call(serde_json::json!({"text": "hi"}), &test_ctx())
            .await;
        assert_eq!(outcome.content(), "hi");
        assert_eq!(cloned.name(), tool.name());
    }

    #[test]
    fn tool_basic() {
        let tool = Tool::new("echo", "Echoes input", |input: Value, _ctx| async move {
            let text = input["text"].as_str().unwrap_or("").to_string();
            ToolResult::success(text)
        })
        .schema(serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}))
        .concurrent(true);

        assert_eq!(tool.name(), "echo");
        assert!(tool.is_concurrent());
    }

    // Layer 1: result-cap helpers

    fn ticket_ctx() -> (
        ToolContext,
        Arc<TicketQueue>,
        String,
        crate::test_util::TempDir,
    ) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        queue.task("seed");
        let key = "TICKET-1".to_string();
        let ctx = test_ctx()
            .ticket_queue(Arc::clone(&queue))
            .ticket_key(key.clone());
        (ctx, queue, key, dir)
    }

    /// An outcome the aggregate cap may write out, which is what those tests
    /// hand it.
    fn stubbable(id: &str, content: &str) -> ToolOutcome {
        ToolOutcome {
            call_id: id.into(),
            content: content.into(),
            failure: None,
            path: None,
        }
    }

    /// A failed outcome, for the tests that pin what the caps leave alone.
    fn failed(id: &str, content: &str) -> ToolOutcome {
        ToolOutcome {
            call_id: id.into(),
            content: content.into(),
            failure: Some(ToolFailureKind::ExecutionFailed),
            path: None,
        }
    }

    fn relative_outputs_path(key: &str, tool_use_id: &str) -> PathBuf {
        PathBuf::from("tickets")
            .join(key)
            .join("outputs")
            .join(format!("{tool_use_id}.txt"))
    }

    fn absolute_outputs_path(dir: &std::path::Path, key: &str, tool_use_id: &str) -> PathBuf {
        dir.join(relative_outputs_path(key, tool_use_id))
    }

    #[test]
    fn write_tool_output_stores_relative_path_in_comment() {
        let (ctx, _queue, key, _dir) = ticket_ctx();
        let mut outcome = stubbable("call-rel", &"z".repeat(500));
        cap_oversized_result(&mut outcome, &ctx, 100);
        let stored = outcome.path.expect("offload happened");
        assert_eq!(stored, relative_outputs_path(&key, "call-rel"));
        assert!(
            stored.is_relative(),
            "comment path must stay portable: {}",
            stored.display()
        );
    }

    #[test]
    fn persisted_output_renders_absolute_path_for_model() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        let mut outcome = stubbable("call-abs", &"y".repeat(500));
        cap_oversized_result(&mut outcome, &ctx, 100);
        let stub = outcome.content;
        let absolute = absolute_outputs_path(dir.path(), &key, "call-abs");
        assert!(
            stub.contains(&absolute.display().to_string()),
            "stub must give the model the joinable on-disk path: {stub}"
        );
    }

    #[test]
    fn cap_oversized_result_passes_through_under_cap() {
        let ctx = test_ctx();
        let mut outcome = stubbable("call-1", "hello");
        cap_oversized_result(&mut outcome, &ctx, 100);
        assert_eq!(outcome.content, "hello");
        assert!(outcome.path.is_none());
    }

    #[test]
    fn cap_oversized_result_replaces_oversized_ok_with_stub() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        let mut outcome = stubbable("call-xyz", &"a".repeat(500));
        cap_oversized_result(&mut outcome, &ctx, 100);
        let stub = outcome.content;
        assert!(stub.starts_with("<persisted-output>"));
        assert!(stub.contains("Output too large"));
        assert!(stub.contains("Full output saved to:"));
        let absolute = absolute_outputs_path(dir.path(), &key, "call-xyz");
        assert!(
            stub.contains(&absolute.display().to_string()),
            "stub must name the absolute path so the model can read the file: {stub}"
        );
        assert!(stub.contains("Preview (first"));
        assert!(stub.ends_with("</persisted-output>"));
        let path = outcome.path.expect("offload path");
        assert_eq!(path, relative_outputs_path(&key, "call-xyz"));
        let body = std::fs::read_to_string(&absolute).unwrap();
        assert_eq!(body, "a".repeat(500));
    }

    #[test]
    fn cap_oversized_result_passes_a_failure_through() {
        let ctx = test_ctx();
        let mut outcome = failed("call-1", "boom");
        cap_oversized_result(&mut outcome, &ctx, 1);
        assert_eq!(outcome.content, "boom");
        assert!(outcome.path.is_none());
    }

    #[test]
    fn cap_oversized_result_returns_raw_when_no_ticket_key() {
        let ctx = test_ctx();
        let payload = "x".repeat(500);
        let mut outcome = stubbable("call-1", &payload);
        cap_oversized_result(&mut outcome, &ctx, 100);
        assert_eq!(outcome.content, payload);
        assert!(outcome.path.is_none(), "no ticket key means no offload");
    }

    #[test]
    fn cap_aggregate_offloads_largest_first() {
        let (ctx, _queue, key, dir) = ticket_ctx();
        // Sizes chosen so the stub's own bytes (~200) don't dominate.
        let small = "a".repeat(40_000);
        let big = "b".repeat(80_000);
        let tiny = "c".repeat(30_000);
        let mut results = vec![
            stubbable("c1", &small),
            stubbable("c2", &big),
            stubbable("c3", &tiny),
        ];
        cap_aggregate_outputs(&mut results, &ctx, 100_000);
        // c2 (the largest) was offloaded; the other two stayed inline.
        assert!(results[1].content.starts_with("<persisted-output>"));
        assert!(results[1].content.contains("Full output saved to:"));
        let big_path = results[1].path.clone().expect("c2 path recorded");
        assert_eq!(big_path, relative_outputs_path(&key, "c2"));
        let body = std::fs::read_to_string(absolute_outputs_path(dir.path(), &key, "c2")).unwrap();
        assert_eq!(body, big);

        assert_eq!(results[0].content.len(), 40_000);
        assert_eq!(results[2].content.len(), 30_000);
        assert!(results[0].path.is_none());
        assert!(results[2].path.is_none());
    }

    #[test]
    fn cap_aggregate_stops_when_only_small_results_remain() {
        let (ctx, _queue, _key, _dir) = ticket_ctx();
        // Many small results whose total far exceeds the cap, but
        // each is already a stub-marked block. Aggregate should bail
        // rather than spin: stubs are skipped, so no candidates.
        let mut results: Vec<ToolOutcome> = (0..5)
            .map(|i| {
                let stub = format!("<persisted-output>already stubbed {i}</persisted-output>");
                stubbable(&format!("c{i}"), &stub)
            })
            .collect();
        let before: Vec<String> = results
            .iter()
            .map(|outcome| outcome.content.clone())
            .collect();
        cap_aggregate_outputs(&mut results, &ctx, 10);
        let after: Vec<String> = results
            .iter()
            .map(|outcome| outcome.content.clone())
            .collect();
        assert_eq!(
            before, after,
            "aggregate must be a no-op when only stubs remain"
        );
    }

    #[test]
    fn format_oversized_tool_result_renders_template() {
        let path = PathBuf::from("/tmp/agentwerk/tickets/TICKET-1/outputs/call-1.txt");
        let stub = format_oversized_tool_result(1_048_576, &path, "preview-body");
        assert!(stub.starts_with("<persisted-output>"));
        assert!(stub.contains("Output too large (1.0 MB)."));
        assert!(stub
            .contains("Full output saved to: /tmp/agentwerk/tickets/TICKET-1/outputs/call-1.txt"));
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
        let mut outcome = stubbable("c1", "");
        replace_empty_output(&mut outcome, "bash");
        assert_eq!(outcome.content, "(bash completed with no output)");
    }

    #[test]
    fn replace_empty_output_passes_non_empty_through() {
        let mut outcome = stubbable("c1", "hello");
        replace_empty_output(&mut outcome, "bash");
        assert_eq!(outcome.content, "hello");
    }

    #[test]
    fn replace_empty_output_passes_a_failure_through() {
        // The guard reads `failure`, not the content, so even an empty
        // failure message is left as the tool reported it.
        let mut outcome = failed("c1", "");
        replace_empty_output(&mut outcome, "bash");
        assert_eq!(outcome.content, "");
    }

    #[test]
    fn cap_aggregate_skips_a_failed_outcome() {
        // A failure's message is what the model must read to recover, so the
        // aggregate cap never writes it out.
        let (ctx, _queue, _key, _dir) = ticket_ctx();
        let mut results = vec![failed("c1", &"e".repeat(50_000))];
        cap_aggregate_outputs(&mut results, &ctx, 10);
        assert_eq!(results[0].content.len(), 50_000);
        assert!(results[0].path.is_none());
    }
}
